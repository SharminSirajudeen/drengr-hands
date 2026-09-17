use super::*;

fn annotated(
    text: &str,
    class: &str,
    bounds: crate::screen::ui_element::Bounds,
) -> AnnotatedElement {
    AnnotatedElement {
        number: 7,
        tap_x: bounds.center_x(),
        tap_y: bounds.center_y(),
        element: crate::screen::ui_element::UiElement {
            class: class.to_string(),
            text: text.to_string(),
            content_desc: String::new(),
            resource_id: String::new(),
            bounds,
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
        },
    }
}

#[test]
fn element_json_carries_bounds_and_state() {
    // Without bounds an agent cannot reason about layout, overlap or where
    // a tap will land; without selected it cannot assert on a radio choice.
    let b = crate::screen::ui_element::Bounds::new(10, 20, 110, 70);
    let mut el = annotated("Weekly", "android.widget.RadioButton", b);
    el.element.selected = true;

    let json = annotated_elements_to_json(&[el]);
    assert_eq!(json[0]["bounds"], json!([10, 20, 110, 70]));
    assert_eq!(json[0]["selected"], json!(true));
    assert_eq!(json[0]["text"], json!("Weekly"));
    assert!(json[0].get("unlabelled").is_none());
    // A tap lands at the centre of bounds, which is what we document.
    assert_eq!((el_center(&json[0])), (60, 45));
}

fn el_center(v: &Value) -> (i64, i64) {
    let b = v["bounds"].as_array().expect("bounds array");
    (
        (b[0].as_i64().unwrap() + b[2].as_i64().unwrap()) / 2,
        (b[1].as_i64().unwrap() + b[3].as_i64().unwrap()) / 2,
    )
}

#[tokio::test]
async fn the_advertised_element_cap_is_actually_read() {
    // max_elements was in the drengr_look schema and read by nothing: a
    // documented knob with no implementation is a placeholder on a user surface.
    fn el(i: i32) -> crate::screen::ui_element::UiElement {
        crate::screen::ui_element::UiElement {
            class: "android.widget.Button".to_string(),
            text: format!("E{i}"),
            content_desc: String::new(),
            resource_id: String::new(),
            bounds: crate::screen::ui_element::Bounds::new(0, i * 60, 100, i * 60 + 40),
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
        }
    }
    let handlers = McpHandlers::new();
    let png = tiny_png();
    let elems: Vec<_> = (0..10).map(el).collect();

    let capped = handlers
        .annotate_stable(&png, &elems, (200, 800), Some(3), "test-device")
        .await
        .expect("annotate");
    assert_eq!(
        capped.elements.len(),
        3,
        "the requested cap must be honoured"
    );

    let uncapped = handlers
        .annotate_stable(&png, &elems, (200, 800), None, "test-device")
        .await
        .expect("annotate");
    assert_eq!(
        uncapped.elements.len(),
        10,
        "no cap requested, no cap applied"
    );
}

#[test]
fn a_disabled_control_says_so() {
    // A disabled element passes is_relevant, so it is in the list. Reporting
    // nothing about it meant an agent tapped a greyed-out button and read the
    // no-op as "stuck".
    let b = || crate::screen::ui_element::Bounds::new(0, 0, 50, 50);
    let live = annotated("Continue", "android.widget.Button", b());
    let mut dead = annotated("Continue", "android.widget.Button", b());
    dead.element.enabled = false;

    let json = annotated_elements_to_json(&[live, dead]);
    assert!(
        json[0].get("disabled").is_none(),
        "a live control carries no flag"
    );
    assert_eq!(json[1]["disabled"], json!(true));
}

#[test]
fn an_unlabelled_element_is_not_findable_by_its_class_name() {
    // find ranked on display_label, which falls back to the class, and then
    // reported text "". So find(target="view") answered match "exact" against
    // a string the response refuses to show: the same fake precision that
    // confidence 1.0 was removed for, reintroduced three commits later.
    let bare = annotated(
        "",
        "android.view.View",
        crate::screen::ui_element::Bounds::new(0, 0, 50, 50),
    );
    assert_eq!(
        bare.element.label_or_empty(),
        "",
        "an unlabelled element has no label to rank against"
    );
    assert!(
        !bare
            .element
            .label_or_empty()
            .to_lowercase()
            .contains("view"),
        "searching 'view' must not match a node whose only 'label' is its class"
    );
}

#[test]
fn unlabelled_element_says_so_instead_of_claiming_view() {
    // An unlabelled clickable used to arrive as text:"View", which is
    // indistinguishable from an element genuinely labelled "View".
    let bare = annotated(
        "",
        "android.view.View",
        crate::screen::ui_element::Bounds::new(0, 0, 50, 50),
    );
    let labelled = annotated(
        "View",
        "android.view.View",
        crate::screen::ui_element::Bounds::new(0, 0, 50, 50),
    );

    let bare_json = annotated_elements_to_json(&[bare]);
    assert_eq!(bare_json[0]["text"], json!(""));
    assert_eq!(bare_json[0]["unlabelled"], json!(true));

    let labelled_json = annotated_elements_to_json(&[labelled]);
    assert_eq!(labelled_json[0]["text"], json!("View"));
    assert!(labelled_json[0].get("unlabelled").is_none());
}

#[test]
fn update_notice_is_announced_once_per_version() {
    // Per-session was already the intent, but every CLI command builds a
    // fresh McpHandlers, so "once" only holds if it survives the process.
    let home = std::env::temp_dir().join(format!("drengr-t{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);

    assert!(
        !update_notice_shown_in(&home, "0.10.7"),
        "a version never announced must not be suppressed"
    );
    mark_update_notice_shown_in(&home, "0.10.7");
    assert!(
        update_notice_shown_in(&home, "0.10.7"),
        "the same version must not be announced twice"
    );
    assert!(
        !update_notice_shown_in(&home, "0.10.8"),
        "a newer version is worth announcing again"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[tokio::test]
async fn an_element_keeps_its_number_when_the_screen_shifts() {
    // The founder's P8: "Support" was n=20, then n=18, then n=21 across renders
    // of the same screen. Positional numbering renumbers everything whenever
    // anything above it appears or disappears.
    fn el(text: &str, top: i32) -> crate::screen::ui_element::UiElement {
        crate::screen::ui_element::UiElement {
            class: "android.widget.Button".to_string(),
            text: text.to_string(),
            content_desc: String::new(),
            resource_id: String::new(),
            bounds: crate::screen::ui_element::Bounds::new(0, top, 100, top + 40),
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
        }
    }

    let handlers = McpHandlers::new();
    let png = tiny_png();

    let first = handlers
        .annotate_stable(
            &png,
            &[el("Home", 0), el("Support", 100)],
            (200, 200),
            None,
            "test-device",
        )
        .await
        .expect("annotate");
    let support_before = first
        .elements
        .iter()
        .find(|e| e.element.text == "Support")
        .expect("Support present")
        .number;

    // A banner appears above it. Positionally Support moves; by identity it does not.
    let second = handlers
        .annotate_stable(
            &png,
            &[el("Banner", 0), el("Home", 50), el("Support", 100)],
            (200, 200),
            None,
            "test-device",
        )
        .await
        .expect("annotate");
    let support_after = second
        .elements
        .iter()
        .find(|e| e.element.text == "Support")
        .expect("Support present")
        .number;

    assert_eq!(
        support_before, support_after,
        "the same element must keep its number across renders"
    );
}

#[test]
fn clickable_without_label_counts_only_unnameable_taps() {
    // The lint the founder wanted from uiautomator's NAF attribute, computed
    // from what Drengr already parses so it reports on every device.
    let b = || crate::screen::ui_element::Bounds::new(0, 0, 50, 50);
    let named = annotated("Submit", "android.widget.Button", b());

    let mut bare = annotated("", "android.view.View", b());
    bare.element.clickable = true;

    let mut decoration = annotated("", "android.view.View", b());
    decoration.element.clickable = false;

    let mut by_id = annotated("", "android.view.View", b());
    by_id.element.clickable = true;
    by_id.element.resource_id = "com.app:id/submit".to_string();

    let count = clickable_without_label(&[named, bare, decoration, by_id]);
    assert_eq!(count, 1, "only the clickable with no text and no id counts");
}

#[test]
fn match_kind_ranks_exact_over_substring() {
    // find used to answer confidence: 1.0 for any substring hit, so a one
    // character match scored the same as an exact one.
    assert_eq!(super::query::query_match_kind("submit", "submit"), "exact");
    assert_eq!(
        super::query::query_match_kind("submit order", "submit"),
        "prefix"
    );
    assert_eq!(
        super::query::query_match_kind("please submit", "submit"),
        "substring"
    );
}

#[test]
fn find_no_longer_reports_a_fabricated_confidence() {
    assert!(
        !include_str!("query.rs").contains("\"confidence\""),
        "confidence was a hardcoded 1.0 for any match and must not return"
    );
}

#[test]
fn the_two_find_paths_share_one_ranker() {
    // The in-memory and on-disk paths ranked matches with their own copy of the
    // same sort. Two copies of a rule is how they drift into different answers
    // depending on whether a screen happens to be in memory.
    let src = include_str!("query.rs");
    // Assert the WIRING, not the implementation. The previous version counted
    // sort_by_key occurrences, which would have gone red if someone replaced
    // the sort with a single-pass min_by_key: a guard that fails when the code
    // improves is inverted.
    assert!(
        src.contains("fn best_label_match"),
        "the shared ranker must exist"
    );
    // Two call sites: the in-memory path and the on-disk path. The generic
    // definition reads best_label_match<T>( so it does not match this pattern.
    assert_eq!(
        src.matches("best_label_match(").count(),
        2,
        "both find paths must rank through the shared helper"
    );
}

#[test]
fn analyze_still_reports_the_accessibility_count() {
    // One call site, so the failure mode is deletion rather than divergence.
    assert!(
        include_str!("query.rs").contains("clickable_without_label"),
        "analyze must keep emitting the a11y count"
    );
}

#[test]
fn swipe_compares_frames_not_encodings() {
    // image_hash on the annotated JPEG versus a raw PNG can never match, so
    // the swipe override fired unconditionally.
    let src = include_str!("do_action.rs");
    assert!(
        !src.contains(&format!("image{}(", "_hash")),
        "compare frames with frames_settled, not byte hashes of different encodings"
    );
}

#[test]
fn every_element_action_resolves_through_one_helper() {
    // tap resolved from disk while long_press did not, so `--element N` worked
    // for one action and failed for the others from a cold CLI.
    assert!(
        !include_str!("do_action.rs").contains("and_then(|a| ScreenAnnotator::tap_coordinates"),
        "every action arm must go through resolve_element"
    );
}

/// Records whether the UI tree was ever requested. `format='grid'` is the
/// fallback for screens whose dump hangs, so it must never wait on one.
struct GridProbe {
    tree_touched: std::sync::atomic::AtomicBool,
}

fn tiny_png() -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::new(64, 64))
        .write_to(&mut buf, image::ImageFormat::Png)
        .expect("encode test png");
    buf.into_inner()
}

#[async_trait::async_trait]
impl DeviceTransport for GridProbe {
    async fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
        Ok(tiny_png())
    }
    async fn ui_tree(&self) -> anyhow::Result<Vec<crate::screen::ui_element::UiElement>> {
        self.tree_touched
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Err(anyhow::anyhow!("adb shell timed out after 10s"))
    }
    async fn screen_size(&self) -> anyhow::Result<(u32, u32)> {
        Ok((1080, 2340))
    }
    async fn current_activity(&self) -> anyhow::Result<String> {
        Ok("dev.example.app/.MainActivity".to_string())
    }
    async fn is_connected(&self) -> bool {
        true
    }
    async fn tap(&self, _: i32, _: i32) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn long_press(&self, _: i32, _: i32, _: u32) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn swipe(
        &self,
        _: crate::screen::ui_element::Point,
        _: crate::screen::ui_element::Point,
        _: u32,
    ) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn type_text(&self, _: &str) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn press_key(&self, _: i32) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn launch_app(&self, _: &str) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn is_app_in_foreground(&self, _: &str) -> anyhow::Result<bool> {
        unimplemented!()
    }
    async fn clear_focused_field(&self) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn device_info(&self) -> anyhow::Result<crate::screen::ui_element::DeviceInfo> {
        unimplemented!()
    }
}

/// Records which trait method each newly registered action reached, so a
/// missing match arm in `handle_do` cannot pass as a wired action.
#[derive(Default)]
pub(super) struct ActionProbe {
    unlocked: std::sync::atomic::AtomicBool,
    accepted: std::sync::atomic::AtomicBool,
    dismissed: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl DeviceTransport for ActionProbe {
    async fn screenshot(&self) -> anyhow::Result<Vec<u8>> {
        Ok(tiny_png())
    }
    async fn ui_tree(&self) -> anyhow::Result<Vec<crate::screen::ui_element::UiElement>> {
        Ok(Vec::new())
    }
    async fn screen_size(&self) -> anyhow::Result<(u32, u32)> {
        Ok((1080, 2340))
    }
    async fn current_activity(&self) -> anyhow::Result<String> {
        Ok("dev.example.app/.MainActivity".to_string())
    }
    async fn is_connected(&self) -> bool {
        true
    }
    async fn unlock(&self) -> anyhow::Result<()> {
        self.unlocked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn alert_text(&self) -> anyhow::Result<Option<String>> {
        Ok(Some("Allow notifications?".to_string()))
    }
    async fn alert_accept(&self) -> anyhow::Result<()> {
        self.accepted
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn alert_dismiss(&self) -> anyhow::Result<()> {
        self.dismissed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }
    async fn app_state(&self, _: &str) -> anyhow::Result<u8> {
        Ok(3)
    }
    async fn tap(&self, _: i32, _: i32) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn long_press(&self, _: i32, _: i32, _: u32) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn swipe(
        &self,
        _: crate::screen::ui_element::Point,
        _: crate::screen::ui_element::Point,
        _: u32,
    ) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn type_text(&self, _: &str) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn press_key(&self, _: i32) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn launch_app(&self, _: &str) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn is_app_in_foreground(&self, _: &str) -> anyhow::Result<bool> {
        unimplemented!()
    }
    async fn clear_focused_field(&self) -> anyhow::Result<()> {
        unimplemented!()
    }
    async fn device_info(&self) -> anyhow::Result<crate::screen::ui_element::DeviceInfo> {
        unimplemented!()
    }
}

async fn probe_do(args: serde_json::Value) -> (String, Arc<ActionProbe>) {
    let handlers = McpHandlers::new();
    let probe = Arc::new(ActionProbe::default());
    handlers
        .transports
        .lock()
        .await
        .insert("probe".to_string(), probe.clone());
    let result = handlers.dispatch("drengr_do", args).await;
    // The image-format reply puts the screenshot at content[0], so read every text slot.
    let body: String = result
        .content
        .iter()
        .filter_map(|c| c.text.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !body.contains("Unknown action"),
        "action was advertised but never dispatched: {body}"
    );
    (body, probe)
}

/// The five capabilities were implemented on the transport but absent from
/// the registry, so no agent could reach them. Each must now run.
#[tokio::test]
async fn unlock_action_reaches_the_transport() {
    let (body, probe) = probe_do(json!({ "action": "unlock" })).await;
    assert!(
        probe.unlocked.load(std::sync::atomic::Ordering::SeqCst),
        "unlock never reached the transport: {body}"
    );
}

#[tokio::test]
async fn alert_text_action_returns_the_dialog_text() {
    let (body, _) = probe_do(json!({ "action": "alert_text" })).await;
    assert!(body.contains("Allow notifications?"), "got: {body}");
}

#[tokio::test]
async fn alert_accept_and_dismiss_reach_their_own_methods() {
    let (_, accepted) = probe_do(json!({ "action": "alert_accept" })).await;
    assert!(accepted.accepted.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!accepted.dismissed.load(std::sync::atomic::Ordering::SeqCst));

    let (_, dismissed) = probe_do(json!({ "action": "alert_dismiss" })).await;
    assert!(dismissed
        .dismissed
        .load(std::sync::atomic::Ordering::SeqCst));
    assert!(!dismissed.accepted.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn app_state_action_names_the_lifecycle_state() {
    let (body, _) = probe_do(json!({ "action": "app_state", "package": "com.example.app" })).await;
    assert!(body.contains("background"), "got: {body}");
}

#[tokio::test]
async fn app_state_action_requires_a_package() {
    let handlers = McpHandlers::new();
    handlers
        .transports
        .lock()
        .await
        .insert("probe".to_string(), Arc::new(ActionProbe::default()));
    let result = handlers
        .dispatch("drengr_do", json!({ "action": "app_state" }))
        .await;
    assert_eq!(result.is_error, Some(true));
}

#[tokio::test]
async fn text_format_does_not_repeat_the_element_array() {
    // 'text' is documented as the cheap format, but it used to return the
    // full elements array AND the text scene, the same data twice.
    let handlers = McpHandlers::new();
    handlers.transports.lock().await.insert(
        "probe".to_string(),
        Arc::new(GridProbe {
            tree_touched: std::sync::atomic::AtomicBool::new(false),
        }),
    );

    let result = handlers
        .dispatch("drengr_look", json!({ "format": "text" }))
        .await;
    let body = result.content[0].text.as_deref().unwrap_or_default();

    assert!(
        body.contains("text_scene"),
        "text format must return a scene"
    );
    assert!(
        !body.contains("\"elements\""),
        "text format must not also carry the element array"
    );
}

#[test]
fn both_do_branches_report_the_same_facts() {
    // tree_error reached the text branch and not the image branch, which is
    // the DEFAULT, so the honesty field was absent from the most-used path.
    // Sibling response shapes must carry the same fields or the caller's view
    // depends on which format they asked for.
    let src = include_str!("do_action.rs");
    assert_eq!(
        src.matches("[\"tree_error\"] = json!(e)").count(),
        2,
        "both the text and image branches of drengr_do must report tree_error"
    );
    assert_eq!(
        src.matches("NO_TREE_HINT").count(),
        2,
        "both branches must carry the hint too"
    );
}

#[tokio::test]
async fn the_element_cap_applies_to_every_format() {
    // max_elements reached the annotator and not the text scene, so 'text'
    // returned 50 elements where 'image' returned the 3 that were asked for.
    fn el(i: i32) -> crate::screen::ui_element::UiElement {
        crate::screen::ui_element::UiElement {
            class: "android.widget.Button".to_string(),
            text: format!("E{i}"),
            content_desc: String::new(),
            resource_id: String::new(),
            bounds: crate::screen::ui_element::Bounds::new(0, i * 60, 100, i * 60 + 40),
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
        }
    }
    let elems: Vec<_> = (0..10).map(el).collect();
    let listed = crate::screen::text_scene::TextSceneBuilder::new(200, 800)
        .build_capped(&elems, 3)
        .description
        .lines()
        .filter(|l| l.starts_with('['))
        .count();
    assert_eq!(
        listed, 3,
        "the text scene must honour the same cap as the annotator"
    );
}

#[test]
fn the_post_action_settle_is_not_a_blind_sleep() {
    // Reverting to sleep(500) left every test green: the settle change had no
    // coverage at all. A behavioural probe would need a device, so this asserts
    // the wiring, which is the part that regressed.
    let src = include_str!("do_action.rs");
    assert!(
        src.contains("wait_for_screen_stable"),
        "the post-action observation must settle before it observes"
    );
    // The property is ordering, not the absence of every sleep: the scroll
    // loops pace themselves with one deliberately.
    let settle = src.find("wait_for_screen_stable").expect("settle call");
    let observe = src.find("transport.observe()").expect("observe call");
    assert!(
        settle < observe,
        "the screen must settle BEFORE the observation, or the frame is mid-transition"
    );
}

#[test]
fn the_update_notice_marker_is_actually_consulted() {
    // The existing test calls the helpers directly, so deleting both call
    // sites from dispatch left it green and the fix unguarded.
    let src = include_str!("mod.rs");
    assert!(
        src.contains("update_notice_already_shown("),
        "dispatch must check the marker before announcing"
    );
    assert!(
        src.contains("mark_update_notice_shown("),
        "dispatch must record the announcement where it is appended"
    );
}

#[tokio::test]
async fn grid_look_never_waits_on_the_ui_tree() {
    let handlers = McpHandlers::new();
    let probe = Arc::new(GridProbe {
        tree_touched: std::sync::atomic::AtomicBool::new(false),
    });
    handlers
        .transports
        .lock()
        .await
        .insert("probe".to_string(), probe.clone());

    let result = handlers
        .dispatch("drengr_look", json!({ "format": "grid" }))
        .await;

    assert_ne!(result.is_error, Some(true), "grid look must succeed");
    assert!(
        !probe.tree_touched.load(std::sync::atomic::Ordering::SeqCst),
        "grid is the fallback for a hanging dump, so it must not request the tree"
    );
}

#[tokio::test]
async fn test_dispatch_unknown_tool() {
    let handlers = McpHandlers::new();
    let result = handlers.dispatch("unknown_tool", json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(result.content[0]
        .text
        .as_deref()
        .unwrap()
        .contains("Unknown tool"));
}

#[tokio::test]
async fn test_handle_look_no_device() {
    let handlers = McpHandlers::new();
    let result = handlers.handle_look(json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(result.content[0]
        .text
        .as_deref()
        .unwrap()
        .contains("No device"));
}

#[tokio::test]
async fn test_handle_do_no_device() {
    let handlers = McpHandlers::new();
    let result = handlers
        .handle_do(json!({"action": "tap", "element": 1}))
        .await;
    assert_eq!(result.is_error, Some(true));
}

#[tokio::test]
async fn test_handle_do_missing_action() {
    let handlers = McpHandlers::new();
    // Set a dummy transport to bypass the "no device" check
    // (In real tests we'd use a mock transport)
    let result = handlers.handle_do(json!({})).await;
    // Should fail with "No device connected" since no transport set
    assert_eq!(result.is_error, Some(true));
}

#[tokio::test]
async fn test_handle_query_missing_question() {
    let handlers = McpHandlers::new();
    let result = handlers.handle_query(json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(result.content[0]
        .text
        .as_deref()
        .unwrap()
        .contains("question"));
}

#[tokio::test]
async fn test_handle_query_unknown_question() {
    let handlers = McpHandlers::new();
    let result = handlers.handle_query(json!({"question": "weather"})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(result.content[0]
        .text
        .as_deref()
        .unwrap()
        .contains("Unknown question"));
}

#[tokio::test]
async fn test_handle_query_find_no_screen() {
    let handlers = McpHandlers::new();
    let result = handlers
        .handle_query(json!({"question": "find", "target": "login"}))
        .await;
    assert_eq!(result.is_error, Some(true));
    assert!(result.content[0]
        .text
        .as_deref()
        .unwrap()
        .contains("No screen observed"));
}

#[tokio::test]
async fn test_all_queries_free_on_free_tier() {
    let handlers = McpHandlers::new();
    // ALL queries are free — no feature gating, only daily usage limit
    for q in &["logcat", "network", "diff", "explore", "assert", "session"] {
        let result = handlers.handle_query(json!({"question": q})).await;
        let text = result.content[0].text.as_deref().unwrap_or("");
        assert!(
            !text.contains("premium_required"),
            "'{}' should be FREE (no feature gates)",
            q
        );
    }
}

#[tokio::test]
async fn test_free_queries_not_gated() {
    let handlers = McpHandlers::new();
    for q in &["devices", "activity", "crash", "keyboard", "app_state"] {
        let result = handlers.handle_query(json!({"question": q})).await;
        let text = result.content[0].text.as_deref().unwrap_or("");
        assert!(
            !text.contains("premium_required"),
            "'{}' should NOT be gated",
            q
        );
    }
}

#[tokio::test]
async fn test_handle_query_find_missing_target() {
    let handlers = McpHandlers::new();
    let result = handlers.handle_query(json!({"question": "find"})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(result.content[0]
        .text
        .as_deref()
        .unwrap()
        .contains("target"));
}

// ─── Only cloud_devices is Pro-gated ─────────────────

#[tokio::test]
async fn test_analyze_not_gated_as_premium_query() {
    let handlers = McpHandlers::new();
    let result = handlers.handle_query(json!({"question": "analyze"})).await;
    let text = result.content[0].text.as_deref().unwrap_or("");
    assert!(
        !text.contains("premium_required"),
        "analyze should not be gated at premium query level (has tiered output)"
    );
}

#[tokio::test]
async fn test_pro_features_single_source_of_truth() {
    // Verify gating uses PRO_FEATURES directly — no separate hardcoded list
    let handlers = McpHandlers::new();
    let non_pro_questions = ["devices", "activity", "crash", "keyboard", "app_state"];
    for q in &non_pro_questions {
        let result = handlers.handle_query(json!({"question": q})).await;
        let text = result.content[0].text.as_deref().unwrap_or("");
        assert!(
            !text.contains("premium_required"),
            "'{}' is NOT in PRO_FEATURES but was gated",
            q
        );
    }
}

// ─── Trial messaging ───────────────────────────────────────────────────

/// Trial banner tests. Uses global TRIAL_CACHE — must not run in parallel
/// with other tests that mutate it. The #[serial] attribute is not available,
/// so we use retry logic to handle contamination from parallel tests.
#[test]
fn friendly_driver_error_runner_not_provisioned() {
    let raw = "runner_not_provisioned — run `drengr build-runner` from your terminal to build the iOS runner (ios18.4-15F31d-runnerabcdef)";
    let out = super::friendly_driver_error(raw).expect("should rewrite");
    assert!(out.contains("iOS runner not built"));
    assert!(out.contains("drengr build-runner"));
    assert!(!out.contains("runner_not_provisioned"));
}

#[test]
fn friendly_driver_error_xcode_missing() {
    let raw = "driver_xcode_missing — install Xcode from developer.apple.com, then run `sudo xcode-select -s /Applications/Xcode.app`";
    let out = super::friendly_driver_error(raw).expect("should rewrite");
    assert!(out.contains("Xcode is not installed"));
    assert!(out.contains("xcode-select"));
}

#[test]
fn friendly_driver_error_sim_runtime_missing_extracts_major() {
    let raw = "driver_sim_runtime_missing ios_major=18 — run `xcodebuild -downloadPlatform iOS`";
    let out = super::friendly_driver_error(raw).expect("should rewrite");
    assert!(out.contains("iOS 18 Simulator runtime"));
    assert!(out.contains("xcodebuild -downloadPlatform iOS"));
}

#[test]
fn friendly_driver_error_passthrough_for_unknown() {
    assert!(super::friendly_driver_error("Screenshot failed: timeout").is_none());
    assert!(super::friendly_driver_error("Tap failed: connection refused").is_none());
}

/// Guards the bug where ACTIONS/capabilities advertised actions that
/// handle_do never dispatched, so agents got "Unknown action" (draw_path,
/// swipe_with_velocity, …). ACTIONS and the dispatch must stay equal.
/// The same guard for drengr_query, which did not have one.
///
/// `drengr_do` has had `every_advertised_action_is_dispatched` for a while and
/// it caught real regressions. `drengr_query` had no equivalent, and that is
/// exactly how `screen_stream` came to be dispatched by `handle_query` while
/// appearing in neither the JSON-Schema enum nor the `capabilities` catalog:
/// no client following documented discovery could ever learn it exists.
#[test]
fn every_advertised_question_is_dispatched() {
    use std::collections::BTreeSet;

    /// Dispatched on purpose without being advertised, with the reason.
    const UNADVERTISED_ON_PURPOSE: &[(&str, &str)] = &[(
        "screen_stream",
        "no transport implements screen_stream_url; advertising a question that can \
             never return a stream would be worse than leaving it undiscoverable. Finishing \
             it means an MJPEG proxy for Android and an endpoint in drengr-runner for iOS, \
             which is its own piece of work, not a line change.",
    )];

    let advertised: BTreeSet<&str> = crate::mcp::capabilities::query_names()
        .into_iter()
        .collect();
    let dispatched: BTreeSet<&str> = super::dispatched_questions().iter().copied().collect();

    let advertised_not_dispatched: Vec<&&str> = advertised.difference(&dispatched).collect();
    assert!(
        advertised_not_dispatched.is_empty(),
        "advertised questions that handle_query does not dispatch (a client calling them \
             gets 'unknown question'): {advertised_not_dispatched:?}"
    );

    let unexplained: Vec<&str> = dispatched
        .difference(&advertised)
        .copied()
        .filter(|q| !UNADVERTISED_ON_PURPOSE.iter().any(|(n, _)| *n == *q))
        .collect();
    assert!(
        unexplained.is_empty(),
        "handle_query dispatches questions nothing advertises, so no client can discover \
             them: {unexplained:?}. Add them to QUERIES in capabilities.rs, or record why not \
             in UNADVERTISED_ON_PURPOSE."
    );
}

#[test]
fn every_advertised_action_is_dispatched() {
    use std::collections::BTreeSet;
    let advertised: BTreeSet<&str> = crate::mcp::actions::action_names().into_iter().collect();
    let dispatched: BTreeSet<&str> = super::dispatched_actions().iter().copied().collect();
    let advertised_not_dispatched: Vec<&&str> = advertised.difference(&dispatched).collect();
    let dispatched_not_advertised: Vec<&&str> = dispatched.difference(&advertised).collect();
    assert!(
            advertised_not_dispatched.is_empty(),
            "ACTIONS advertises actions handle_do doesn't dispatch (-> 'Unknown action'): {advertised_not_dispatched:?}"
        );
    assert!(
        dispatched_not_advertised.is_empty(),
        "handle_do dispatches actions not in ACTIONS: {dispatched_not_advertised:?}"
    );
}

#[tokio::test]
async fn test_handle_query_logcat_no_device() {
    let handlers = McpHandlers::new();
    let result = handlers
        .handle_query(json!({"question": "logcat", "package": "com.test"}))
        .await;
    assert_eq!(result.is_error, Some(true));
    assert!(result.content[0]
        .text
        .as_deref()
        .unwrap()
        .contains("No device"));
}

#[tokio::test]
async fn test_handle_query_logcat_no_package() {
    let handlers = McpHandlers::new();
    let result = handlers.handle_query(json!({"question": "logcat"})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(result.content[0]
        .text
        .as_deref()
        .unwrap()
        .contains("No device"));
}
