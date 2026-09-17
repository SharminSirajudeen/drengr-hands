use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::AppiumTransport;
use crate::transport::DeviceTransport;

/// A hub that answers `route` with an HTTP failure, for the paths whose whole
/// job is to explain what went wrong.
async fn failing_hub(
    platform: &str,
    verb: &str,
    route: &str,
    status: u16,
    body: Value,
) -> (MockServer, AppiumTransport) {
    let server = MockServer::start().await;
    Mock::given(method(verb))
        .and(path(format!("{SID}{route}")))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    let transport = AppiumTransport::for_test(&server.uri(), platform);
    (server, transport)
}

fn driver_error(code: &str, message: &str) -> Value {
    json!({ "value": { "error": code, "message": message } })
}

const SID: &str = "/session/test-session";

async fn hub(verb: &str, route: &str, value: Value) -> (MockServer, AppiumTransport) {
    hub_for("Android", verb, route, value).await
}

async fn hub_for(
    platform: &str,
    verb: &str,
    route: &str,
    value: Value,
) -> (MockServer, AppiumTransport) {
    let server = MockServer::start().await;
    Mock::given(method(verb))
        .and(path(format!("{SID}{route}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": value })))
        .mount(&server)
        .await;
    let transport = AppiumTransport::for_test(&server.uri(), platform);
    (server, transport)
}

/// Body of the most recent request to `route`, ignoring anything else the
/// server happened to receive.
async fn last_body_for(server: &MockServer, route: &str) -> Value {
    let reqs = server.received_requests().await.expect("recorded requests");
    let want = format!("{SID}{route}");
    reqs.iter()
        .rev()
        .find(|r| r.url.path() == want)
        .map(|r| serde_json::from_slice(&r.body).expect("json body"))
        .unwrap_or_else(|| panic!("no request recorded for {route}"))
}

#[tokio::test]
async fn android_clipboard_is_base64_decoded() {
    // Appium returns clipboard content base64-encoded; handing that string back
    // raw would look like a successful read of the wrong text.
    let encoded = "aGVsbG8gd29ybGQ="; // "hello world"
    let (_s, t) = hub("POST", "/execute/sync", json!(encoded)).await;
    assert_eq!(t.pasteboard_get().await.unwrap(), "hello world");
}

#[tokio::test]
async fn android_clipboard_write_is_base64_encoded() {
    let (server, t) = hub("POST", "/execute/sync", Value::Null).await;
    t.pasteboard_set("hello world").await.unwrap();
    let body = last_body_for(&server, "/execute/sync").await;
    assert_eq!(body["script"], "mobile: setClipboard");
    assert_eq!(body["args"][0]["content"], "aGVsbG8gd29ybGQ=");
}

#[tokio::test]
async fn clipboard_rejects_content_that_is_not_base64() {
    let (_s, t) = hub("POST", "/execute/sync", json!("not-!-base64")).await;
    assert!(t.pasteboard_get().await.is_err());
}

#[tokio::test]
async fn no_open_alert_reads_as_none_not_as_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{SID}/alert/text")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "value": {"error": "no such alert", "message": "no such alert"}
        })))
        .mount(&server)
        .await;
    let t = AppiumTransport::for_test(&server.uri(), "Android");
    assert_eq!(t.alert_text().await.unwrap(), None);
}

#[tokio::test]
async fn a_showing_alert_returns_its_text() {
    let (_s, t) = hub("GET", "/alert/text", json!("Allow location?")).await;
    assert_eq!(
        t.alert_text().await.unwrap(),
        Some("Allow location?".into())
    );
}

#[tokio::test]
async fn a_broken_alert_query_is_an_error_not_a_none() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{SID}/alert/text")))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"value": {"error": "boom"}})))
        .mount(&server)
        .await;
    let t = AppiumTransport::for_test(&server.uri(), "Android");
    assert!(t.alert_text().await.is_err());
}

#[tokio::test]
async fn app_state_reports_the_drivers_lifecycle_number() {
    let (_s, t) = hub("POST", "/appium/device/app_state", json!(4)).await;
    assert_eq!(t.app_state("com.example.app").await.unwrap(), 4);
    assert!(t.is_app_in_foreground("com.example.app").await.unwrap());
}

#[tokio::test]
async fn app_state_without_a_number_is_an_error_not_a_zero() {
    let (_s, t) = hub("POST", "/appium/device/app_state", Value::Null).await;
    assert!(t.app_state("com.example.app").await.is_err());
}

#[tokio::test]
async fn unreadable_screen_size_errors_rather_than_guessing() {
    // Guessing 1080x2340 would silently misplace every coordinate we derive.
    let (_s, t) = hub("GET", "/window/current/size", json!({})).await;
    assert!(t.screen_size().await.is_err());
}

#[tokio::test]
async fn screen_size_reads_the_real_dimensions() {
    let (_s, t) = hub(
        "GET",
        "/window/current/size",
        json!({"width": 1170, "height": 2532}),
    )
    .await;
    assert_eq!(t.screen_size().await.unwrap(), (1170, 2532));
}

#[tokio::test]
async fn ios_rejects_a_keycode_it_cannot_send() {
    let (_s, t) = hub_for("iOS", "POST", "/back", Value::Null).await;
    // TAB has no XCUITest equivalent — reporting success would be a lie.
    assert!(t.press_key(crate::transport::keycode::TAB).await.is_err());
    assert!(t.press_key(crate::transport::keycode::BACK).await.is_ok());
}

#[tokio::test]
async fn orientation_keeps_the_two_landscapes_apart() {
    let (server, t) = hub("POST", "/rotation", Value::Null).await;
    t.set_orientation(3).await.unwrap();
    assert_eq!(last_body_for(&server, "/rotation").await["z"], 270);
    t.set_orientation(1).await.unwrap();
    assert_eq!(last_body_for(&server, "/rotation").await["z"], 90);
    assert!(t.set_orientation(9).await.is_err());
}

#[tokio::test]
async fn stop_recording_writes_the_decoded_video_to_disk() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clip.mp4").to_string_lossy().to_string();

    let (_s, t) = hub("POST", "/appium/stop_recording_screen", json!("aGVsbG8=")).await;
    *t.recording.lock().unwrap() = Some(target.clone());

    let returned = t.stop_recording().await.unwrap();
    assert_eq!(returned, target);
    assert_eq!(std::fs::read(&target).unwrap(), b"hello");
}

#[tokio::test]
async fn stop_recording_without_a_payload_does_not_claim_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clip.mp4").to_string_lossy().to_string();

    let (_s, t) = hub("POST", "/appium/stop_recording_screen", json!("")).await;
    *t.recording.lock().unwrap() = Some(target.clone());

    assert!(t.stop_recording().await.is_err());
    assert!(!std::path::Path::new(&target).exists());
}

#[tokio::test]
async fn stop_recording_with_nothing_running_is_an_error() {
    let (_s, t) = hub("POST", "/appium/stop_recording_screen", json!("aGk=")).await;
    assert!(t.stop_recording().await.is_err());
}

#[tokio::test]
async fn removing_an_app_that_was_not_there_is_reported_not_swallowed() {
    let (_s, t) = hub("POST", "/appium/device/remove_app", json!(false)).await;
    assert!(t.uninstall_app("com.example.app").await.is_err());
}

#[tokio::test]
async fn terminating_an_app_that_was_not_running_is_reported() {
    let (_s, t) = hub("POST", "/appium/device/terminate_app", json!(false)).await;
    assert!(t.terminate_app("com.example.app").await.is_err());
}

#[tokio::test]
async fn app_commands_use_the_platforms_identifier_key() {
    let (server, android) = hub("POST", "/appium/device/activate_app", Value::Null).await;
    android.launch_app("com.example.app").await.unwrap();
    assert_eq!(
        last_body_for(&server, "/appium/device/activate_app").await["appId"],
        "com.example.app"
    );

    let (server, ios) = hub_for("iOS", "POST", "/appium/device/activate_app", Value::Null).await;
    ios.launch_app("com.example.app").await.unwrap();
    assert_eq!(
        last_body_for(&server, "/appium/device/activate_app").await["bundleId"],
        "com.example.app"
    );
}

#[tokio::test]
async fn installed_app_listing_parses_package_lines() {
    let (_s, t) = hub(
        "POST",
        "/execute/sync",
        json!("package:com.example.one\npackage:com.example.two\n"),
    )
    .await;
    assert_eq!(
        t.list_installed_apps().await.unwrap(),
        vec!["com.example.one", "com.example.two"]
    );
}

#[tokio::test]
async fn capabilities_absent_on_ios_fail_loudly() {
    let (_s, t) = hub_for("iOS", "POST", "/execute/sync", Value::Null).await;
    // Neither of these exists for a remote XCUITest session; both must say so
    // rather than return an empty list or a silent Ok.
    assert!(t.list_installed_apps().await.is_err());
    assert!(t.clear_app_data("com.example.app").await.is_err());
}

#[tokio::test]
async fn draw_path_is_one_continuous_stroke() {
    let (server, t) = hub("POST", "/actions", Value::Null).await;
    let pts = [
        crate::screen::ui_element::Point { x: 0, y: 0 },
        crate::screen::ui_element::Point { x: 10, y: 10 },
        crate::screen::ui_element::Point { x: 20, y: 30 },
    ];
    t.draw_path(&pts, 300).await.unwrap();

    let steps = last_body_for(&server, "/actions").await["actions"][0]["actions"].clone();
    let steps = steps.as_array().unwrap();
    let downs = steps.iter().filter(|s| s["type"] == "pointerDown").count();
    let ups = steps.iter().filter(|s| s["type"] == "pointerUp").count();
    // A segmented fallback would lift the pen between every pair of points.
    assert_eq!((downs, ups), (1, 1));
    assert_eq!(
        steps.iter().filter(|s| s["type"] == "pointerMove").count(),
        3
    );
}

#[tokio::test]
async fn http_logs_are_parsed_from_appium_log_entries() {
    let (_s, t) = hub(
        "POST",
        "/log",
        json!([
            {"timestamp": 1, "level": "ALL", "message": "01-01 10:00:00.000 1 1 I okhttp.OkHttpClient: --> GET https://api.example.com/v1/me"},
            {"timestamp": 2, "level": "ALL", "message": "01-01 10:00:01.000 1 1 I okhttp.OkHttpClient: <-- 200 https://api.example.com/v1/me (120ms)"}
        ]),
    )
    .await;

    let events = t.capture_http_logs().await.unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].status, Some(200));
    assert_eq!(events[0].method.as_deref(), Some("GET"));
    assert_eq!(events[0].url, "https://api.example.com/v1/me");
}

#[tokio::test]
async fn a_crash_line_is_found_for_the_right_package_only() {
    let (_s, t) = hub(
        "POST",
        "/log",
        json!([
            {"timestamp": 1, "level": "ALL", "message": "01-01 10:00:00.000 1 1 E AndroidRuntime: FATAL EXCEPTION: main Process: com.example.app"}
        ]),
    )
    .await;
    assert!(t.check_crash_logcat("com.example.app").await.unwrap());
    assert!(!t.check_crash_logcat("com.other.app").await.unwrap());
}

#[tokio::test]
async fn an_unreachable_log_is_an_error_not_a_clean_bill_of_health() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{SID}/log")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let t = AppiumTransport::for_test(&server.uri(), "Android");
    assert!(t.check_crash_logcat("com.example.app").await.is_err());
}

#[tokio::test]
async fn readers_of_the_device_log_do_not_steal_each_others_lines() {
    // Appium's getLog drains the driver buffer, so the retained buffer is what
    // lets a crash check and a network capture both see the same traffic.
    let server = MockServer::start().await;
    // The driver hands each line over exactly once, then has nothing left.
    Mock::given(method("POST"))
        .and(path(format!("{SID}/log")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": [
            {"timestamp": 1, "level": "ALL", "message": "01-01 10:00:01.000 1 1 I okhttp.OkHttpClient: <-- 200 https://api.example.com/a (5ms)"}
        ]})))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{SID}/log")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&server)
        .await;

    let t = AppiumTransport::for_test(&server.uri(), "Android");
    assert!(!t.check_crash_logcat("com.example.app").await.unwrap());
    // Without the retained buffer the crash check above would have eaten this.
    assert_eq!(t.capture_http_logs().await.unwrap().len(), 1);

    t.clear_http_logs().await.unwrap();
    assert!(t.capture_http_logs().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_bad_package_name_never_reaches_the_wire() {
    let (server, t) = hub("POST", "/appium/device/activate_app", Value::Null).await;
    assert!(t.launch_app("com.example.app; rm -rf /").await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn open_url_rejects_a_scheme_we_will_not_send() {
    let (server, t) = hub("POST", "/url", Value::Null).await;
    assert!(t.open_url("file:///etc/passwd").await.is_err());
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn android_cannot_fake_a_rejected_fingerprint() {
    let (_s, t) = hub("POST", "/execute/sync", Value::Null).await;
    assert!(t.simulate_biometric(true).await.is_ok());
    // The emulator can only replay an enrolled finger; claiming a failed match
    // would be inventing a result.
    assert!(t.simulate_biometric(false).await.is_err());
}

#[tokio::test]
async fn out_of_range_coordinates_are_refused() {
    let (_s, t) = hub("POST", "/location", Value::Null).await;
    assert!(t.set_location(37.77, -122.41).await.is_ok());
    assert!(t.set_location(91.0, 0.0).await.is_err());
    assert!(t.set_location(0.0, 181.0).await.is_err());
}

#[tokio::test]
async fn appearance_uses_the_right_command_per_platform() {
    let (server, android) = hub("POST", "/execute/sync", Value::Null).await;
    android.set_appearance(true).await.unwrap();
    let body = last_body_for(&server, "/execute/sync").await;
    assert_eq!(body["script"], "mobile: setUiMode");
    assert_eq!(body["args"][0]["value"], "yes");

    let (server, ios) = hub_for("iOS", "POST", "/execute/sync", Value::Null).await;
    ios.set_appearance(true).await.unwrap();
    let body = last_body_for(&server, "/execute/sync").await;
    assert_eq!(body["script"], "mobile: setAppearance");
    assert_eq!(body["args"][0]["style"], "dark");
}

/// A hub answering the three calls `death_report` makes.
async fn death_hub(state: i64, log: Value) -> (MockServer, AppiumTransport) {
    let server = MockServer::start().await;
    for (verb, route, value) in [
        ("GET", "".to_string(), json!({})),
        ("POST", "/appium/device/app_state".to_string(), json!(state)),
        ("POST", "/log".to_string(), log),
    ] {
        Mock::given(method(verb))
            .and(path(format!("{SID}{route}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "value": value })))
            .mount(&server)
            .await;
    }
    let t = AppiumTransport::for_test(&server.uri(), "Android");
    (server, t)
}

#[tokio::test]
async fn a_stopped_app_is_not_called_a_clean_exit_without_evidence() {
    // app_state alone cannot tell a tidy shutdown from a crash. Naming one is
    // a fabricated reason, which is worse than admitting we do not know.
    let (_s, t) = death_hub(1, json!([])).await;
    let (reason, detail) = t.death_report("com.example.app").await;
    assert_eq!(reason, "unknown");
    assert!(detail.unwrap().contains("no crash line"));
}

#[tokio::test]
async fn a_stopped_app_with_a_crash_line_is_reported_as_crashed() {
    let (_s, t) = death_hub(
        1,
        json!([
            {"timestamp": 1, "level": "ALL", "message": "01-01 10:00:00.000 1 1 E AndroidRuntime: FATAL EXCEPTION: main Process: com.example.app"}
        ]),
    )
    .await;
    assert_eq!(t.death_report("com.example.app").await.0, "crashed");
}

#[tokio::test]
async fn a_foreground_app_is_reported_as_running() {
    let (_s, t) = death_hub(4, json!([])).await;
    assert_eq!(t.death_report("com.example.app").await.0, "running");
}

#[tokio::test]
async fn a_closed_session_refuses_to_act() {
    let mut t = AppiumTransport::for_test("http://127.0.0.1:1", "Android");
    t.session_id = None;
    assert!(t.tap(1, 1).await.is_err());
    assert!(!t.is_connected().await);
}

// ---------------------------------------------------------------------------
// B1 — the device this transport drives
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cloud_device_reports_an_identity_at_all() {
    // `""` made every cross-process element lookup miss: `observation_for_device`
    // refuses a record it cannot attribute, so `look` then `do --element 3` in a
    // second process never resolved on cloud.
    let t = AppiumTransport::for_test("http://127.0.0.1:1", "Android");
    assert!(!t.id().is_empty());
    assert!(
        crate::validate::is_valid_device_id(t.id()),
        "{} would be rejected as a transport map key",
        t.id()
    );
}

#[tokio::test]
async fn two_sessions_on_the_same_device_model_are_not_one_device() {
    // Same model, different session. Sharing an identity would let stale tap
    // targets from a finished session resolve against a live one.
    let a = AppiumTransport::for_session("http://127.0.0.1:1", "Android", "session-a");
    let b = AppiumTransport::for_session("http://127.0.0.1:1", "Android", "session-b");
    assert_ne!(a.id(), b.id());
}

#[tokio::test]
async fn the_identity_holds_still_for_the_life_of_the_session() {
    let a = AppiumTransport::for_session("http://127.0.0.1:1", "Android", "session-a");
    let again = AppiumTransport::for_session("http://127.0.0.1:1", "Android", "session-a");
    assert_eq!(a.id(), again.id());
    assert_eq!(a.id(), a.id());
}

#[tokio::test]
async fn the_session_token_is_not_the_identity() {
    // The session id is a bearer token: with the hub URL it grants control of a
    // billed device. The identity is written to ~/.drengr, so it carries a
    // digest instead.
    let secret = "9f3c1d2e-secret-session-token";
    let t = AppiumTransport::for_session("http://127.0.0.1:1", "iOS", secret);
    assert!(
        !t.id().contains(secret),
        "session token leaked into the device identity: {}",
        t.id()
    );
    assert!(!t.id().contains("secret"));
}

#[tokio::test]
async fn one_device_has_one_identity() {
    // device_info() keys the transport map, id() stamps observations. Two
    // different strings is how a record written under one is looked up under
    // the other and never matches.
    let (_s, t) = hub("GET", "", Value::Null).await;
    assert_eq!(t.device_info().await.unwrap().id, t.id());
}

// ---------------------------------------------------------------------------
// B3 — the seven live-run risks, each naming its endpoint
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_refused_rotation_names_the_route_it_tried() {
    let (_s, t) = failing_hub(
        "iOS",
        "POST",
        "/rotation",
        405,
        driver_error("unknown command", "POST /rotation is not implemented"),
    )
    .await;
    let e = format!("{:#}", t.set_orientation(3).await.unwrap_err());
    assert!(e.contains("/rotation"), "{e}");
    assert!(e.contains("orientation"), "{e}");
    assert!(e.contains("A01 B3"), "{e}");
}

#[tokio::test]
async fn a_failure_that_merely_mentions_alerts_is_not_read_as_no_alert() {
    // The old substring match treated any message containing "no alert" as
    // proof that nothing was showing, so a dead session reported a clean screen.
    let (_s, t) = failing_hub(
        "Android",
        "GET",
        "/alert/text",
        500,
        driver_error(
            "unknown error",
            "device disconnected, no alert state available",
        ),
    )
    .await;
    let e = format!("{:#}", t.alert_text().await.unwrap_err());
    assert!(e.contains("/alert/text"), "{e}");
    assert!(e.contains("no such alert"), "{e}");
}

#[tokio::test]
async fn the_legacy_jsonwp_no_alert_status_still_reads_as_no_alert() {
    let (_s, t) = failing_hub(
        "Android",
        "GET",
        "/alert/text",
        400,
        json!({ "status": 27, "value": { "message": "no alert open" } }),
    )
    .await;
    assert_eq!(t.alert_text().await.unwrap(), None);
}

#[tokio::test]
async fn accepting_nothing_says_so_instead_of_reporting_a_wire_fault() {
    let (_s, t) = failing_hub(
        "iOS",
        "POST",
        "/alert/accept",
        404,
        driver_error("no such alert", ""),
    )
    .await;
    let e = format!("{:#}", t.alert_accept().await.unwrap_err());
    assert!(e.contains("No alert is showing"), "{e}");
}

#[tokio::test]
async fn a_broken_accept_names_the_route_and_the_capability_that_defeats_it() {
    let (_s, t) = failing_hub(
        "iOS",
        "POST",
        "/alert/dismiss",
        500,
        driver_error("unknown error", "wda crashed"),
    )
    .await;
    let e = format!("{:#}", t.alert_dismiss().await.unwrap_err());
    assert!(e.contains("/alert/dismiss"), "{e}");
    assert!(e.contains("autoAcceptAlerts"), "{e}");
}

#[tokio::test]
async fn a_refused_unlock_names_the_legacy_route_and_its_replacement() {
    let (_s, t) = failing_hub(
        "Android",
        "POST",
        "/appium/device/unlock",
        404,
        driver_error("unknown command", "not implemented"),
    )
    .await;
    let e = format!("{:#}", t.unlock().await.unwrap_err());
    assert!(e.contains("/appium/device/unlock"), "{e}");
    assert!(e.contains("mobile: unlock"), "{e}");
}

#[tokio::test]
async fn an_unreachable_log_names_both_the_legacy_and_the_appium_3_route() {
    let (_s, t) = failing_hub(
        "Android",
        "POST",
        "/log",
        404,
        driver_error("unknown command", "not implemented"),
    )
    .await;
    let e = format!("{:#}", t.capture_http_logs().await.unwrap_err());
    assert!(e.contains("/session/:id/log"), "{e}");
    assert!(e.contains("/se/log"), "{e}");
    assert!(e.contains("A01 B3"), "{e}");
}

#[tokio::test]
async fn log_entries_in_an_unreadable_shape_are_not_an_empty_healthy_log() {
    // Entries whose text is under a different key used to be filtered away
    // silently, leaving the crash check and the network capture both reporting
    // nothing wrong on a device that was talking.
    let (_s, t) = hub(
        "POST",
        "/log",
        json!([{ "timestamp": 1, "level": "ALL", "text": "01-01 10:00:00.000 1 1 I Tag: hello" }]),
    )
    .await;
    let e = format!("{:#}", t.capture_http_logs().await.unwrap_err());
    assert!(e.contains("/session/:id/log"), "{e}");
    assert!(e.contains("message"), "{e}");
    assert!(e.contains("level, text, timestamp"), "{e}");
}

#[tokio::test]
async fn a_genuinely_quiet_log_is_still_quiet_not_an_error() {
    let (_s, t) = hub("POST", "/log", json!([])).await;
    assert!(t.capture_http_logs().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_refused_recorder_names_the_route_and_who_disables_it() {
    let (_s, t) = failing_hub(
        "Android",
        "POST",
        "/appium/start_recording_screen",
        500,
        driver_error("unknown error", "screen recording is disabled"),
    )
    .await;
    let e = format!("{:#}", t.start_recording().await.unwrap_err());
    assert!(e.contains("/appium/start_recording_screen"), "{e}");
    assert!(e.contains("BrowserStack"), "{e}");
}

#[tokio::test]
async fn an_empty_video_payload_names_the_route_that_produced_it() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("clip.mp4").to_string_lossy().to_string();
    let (_s, t) = hub("POST", "/appium/stop_recording_screen", json!("")).await;
    *t.recording.lock().unwrap() = Some(target);
    let e = format!("{:#}", t.stop_recording().await.unwrap_err());
    assert!(e.contains("/appium/stop_recording_screen"), "{e}");
}

#[tokio::test]
async fn a_base64_answer_to_a_utf8_pasteboard_request_is_refused_not_returned() {
    // XCUITest is asked for utf8. A build that answers in base64 would otherwise
    // hand "aGVsbG8gd29ybGQ=" back as if that were the clipboard's contents.
    let (_s, t) = hub_for("iOS", "POST", "/execute/sync", json!("aGVsbG8gd29ybGQ=")).await;
    let e = format!("{:#}", t.pasteboard_get().await.unwrap_err());
    assert!(e.contains("mobile: getPasteboard"), "{e}");
    assert!(e.contains("base64"), "{e}");
    assert!(e.contains("utf8"), "{e}");
}

#[tokio::test]
async fn ordinary_pasteboard_text_comes_back_untouched() {
    let (_s, t) = hub_for("iOS", "POST", "/execute/sync", json!("hello world")).await;
    assert_eq!(t.pasteboard_get().await.unwrap(), "hello world");
    let (_s, t) = hub_for("iOS", "POST", "/execute/sync", json!("")).await;
    assert_eq!(t.pasteboard_get().await.unwrap(), "");
}

#[tokio::test]
async fn a_pasteboard_the_device_will_not_serve_says_why() {
    let (_s, t) = failing_hub(
        "iOS",
        "POST",
        "/execute/sync",
        500,
        driver_error("unsupported operation", "Not supported on real devices"),
    )
    .await;
    let e = format!("{:#}", t.pasteboard_get().await.unwrap_err());
    assert!(e.contains("mobile: getPasteboard"), "{e}");
    assert!(e.contains("Simulator-only"), "{e}");
}

// ---------------------------------------------------------------------------
// B4 — a refusal names the capability it is missing
// ---------------------------------------------------------------------------

#[tokio::test]
async fn every_capability_gated_refusal_names_the_capability() {
    let (_s, ios) = failing_hub(
        "iOS",
        "POST",
        "/execute/sync",
        500,
        driver_error("unsupported operation", "not available"),
    )
    .await;
    for (what, err) in [
        (
            "uiautomator2:adb_shell",
            ios.list_installed_apps().await.unwrap_err(),
        ),
        (
            "mobile: clearApp",
            ios.clear_app_data("com.example.app").await.unwrap_err(),
        ),
        (
            "mobile: setPermission",
            ios.grant_permission("android.permission.CAMERA", "com.example.app")
                .await
                .unwrap_err(),
        ),
        (
            "mobile: touchId",
            ios.simulate_biometric(true).await.unwrap_err(),
        ),
    ] {
        let text = format!("{err:#}");
        assert!(text.contains(what), "refusal does not name {what}: {text}");
    }

    let (_s, android) = failing_hub(
        "Android",
        "POST",
        "/execute/sync",
        500,
        driver_error("unknown command", "adb_shell is not enabled"),
    )
    .await;
    let text = format!("{:#}", android.list_installed_apps().await.unwrap_err());
    assert!(text.contains("uiautomator2:adb_shell"), "{text}");
}
