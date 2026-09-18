// Single source of truth for MCP `drengr_do` actions.
//
// The schema enum in `tools.rs`, the dispatch in `handlers.rs`, the
// `drengr_query("capabilities")` response, and the hint-engine deny-list
// all read from `ACTIONS` below. New action = add one entry; nothing
// else can drift.
//
// Security tier (per audit MCPCAP review):
//   - destructive: alters device state in ways that are hard to undo
//     (uninstall app, wipe data, install untrusted code, follow URL).
//     The hint engine must NEVER suggest a destructive action — even if
//     it would solve the LLM's stated problem — to avoid weaponizing
//     prompt-injection from on-device app text.

#[derive(Debug, Clone, Copy)]
pub struct ActionDef {
    pub name: &'static str,
    pub purpose: &'static str,
    pub required: &'static [&'static str],
    pub optional: &'static [&'static str],
    pub platforms: &'static [&'static str],
    pub destructive: bool,
    pub example: &'static str,
}

const BOTH: &[&str] = &["android", "ios"];
const ANDROID: &[&str] = &["android"];
const IOS: &[&str] = &["ios"];

pub static ACTIONS: &[ActionDef] = &[
    // --- Element interaction (safe) ---
    ActionDef {
        name: "tap",
        purpose: "Tap a numbered element on the screen.",
        required: &["element"],
        optional: &["element_text", "scroll_to_find", "max_scroll"],
        platforms: BOTH,
        destructive: false,
        example: "tap(element=3)",
    },
    ActionDef {
        name: "type",
        purpose: "Type text. Optionally taps an element first to focus it.",
        required: &["text"],
        optional: &["element"],
        platforms: BOTH,
        destructive: false,
        example: "type(text='hello world', element=2)",
    },
    ActionDef {
        name: "clear_and_type",
        purpose: "Clear the focused field then type text.",
        required: &["text"],
        optional: &["element"],
        platforms: BOTH,
        destructive: false,
        example: "clear_and_type(text='new value')",
    },
    ActionDef {
        name: "long_press",
        purpose: "Long-press a numbered element (typically opens a context menu).",
        required: &["element"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "long_press(element=5)",
    },

    // --- Gesture (safe) ---
    ActionDef {
        name: "swipe",
        purpose: "Swipe in a cardinal direction.",
        required: &["direction"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "swipe(direction='up')",
    },
    ActionDef {
        name: "swipe_with_velocity",
        purpose: "Swipe with explicit velocity for fast-fling carousels and inertial lists.",
        required: &["direction"],
        optional: &["velocity"],
        platforms: BOTH,
        destructive: false,
        example: "swipe_with_velocity(direction='left', velocity=2000)",
    },
    ActionDef {
        name: "draw_path",
        purpose: "Draw a multi-point path (signatures, freehand sketches, slow gestures). Continuous on iOS, segmented on Android.",
        required: &["points"],
        optional: &["duration_ms"],
        platforms: BOTH,
        destructive: false,
        example: "draw_path(points=[[100,100],[200,200],[300,150]], duration_ms=600)",
    },
    ActionDef {
        name: "scroll_to_top",
        purpose: "Scroll the current scrollable to the top.",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "scroll_to_top()",
    },
    ActionDef {
        name: "scroll_to_bottom",
        purpose: "Scroll the current scrollable to the bottom.",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "scroll_to_bottom()",
    },

    // --- Navigation (safe) ---
    ActionDef {
        name: "back",
        purpose: "Press the back button (hardware on Android, swipe-from-edge on iOS).",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "back()",
    },
    ActionDef {
        name: "home",
        purpose: "Press the home button.",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "home()",
    },
    ActionDef {
        name: "go_home",
        purpose: "Go to home screen via the platform's preferred mechanism.",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "go_home()",
    },
    ActionDef {
        name: "key",
        purpose: "Send a named or numeric key. Named: 'enter', 'delete', 'tab', 'move_end', 'back', 'home'. Or a numeric Android keycode.",
        required: &["keycode"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "key(keycode='enter')",
    },

    // --- App control ---
    ActionDef {
        name: "launch",
        purpose: "Launch an app by exact package/bundle id (e.g. 'com.example.app').",
        required: &["package"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "launch(package='com.android.settings')",
    },
    ActionDef {
        name: "launch_app",
        purpose: "Launch an app by display name (fuzzy-matches installed apps; preferred over `launch` when the user said 'open Settings').",
        required: &["target"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "launch_app(target='Settings')",
    },
    ActionDef {
        name: "list_installed_apps",
        purpose: "Return the list of installed apps with display names. Useful when fuzzy match needs disambiguation.",
        required: &[],
        optional: &["app_kind"],
        platforms: BOTH,
        destructive: false,
        example: "list_installed_apps(app_kind='user')",
    },
    ActionDef {
        name: "spotlight_search",
        purpose: "Open iOS Spotlight (home + pull-down) and type a query. Use to find/launch apps or content by name.",
        required: &["query"],
        optional: &[],
        platforms: IOS,
        destructive: false,
        example: "spotlight_search(query='Maps')",
    },

    // --- Wait / observation ---
    ActionDef {
        name: "wait",
        purpose: "Pause or poll for a condition. Pass `until='stable'`, `until='element:TEXT'`, `until='network:idle'`, or omit for a 1s pause.",
        required: &[],
        optional: &["until", "timeout"],
        platforms: BOTH,
        destructive: false,
        example: "wait(until='stable', timeout=10)",
    },

    // --- Destructive: requires deliberate use, hint engine never suggests these ---
    ActionDef {
        name: "install",
        purpose: "Install an APK or app bundle from disk path.",
        required: &["apk"],
        optional: &["package"],
        platforms: BOTH,
        destructive: true,
        example: "install(apk='/path/to/app.apk', package='com.example')",
    },
    ActionDef {
        name: "uninstall",
        purpose: "Uninstall an app by package/bundle id. Permanent.",
        required: &["package"],
        optional: &[],
        platforms: BOTH,
        destructive: true,
        example: "uninstall(package='com.example.app')",
    },
    ActionDef {
        name: "terminate_app",
        purpose: "Force-quit a running app. State is lost.",
        required: &["package"],
        optional: &[],
        platforms: BOTH,
        destructive: true,
        example: "terminate_app(package='com.example.app')",
    },
    ActionDef {
        name: "clear_app_data",
        purpose: "Wipe an app's data directory (cookies, prefs, files). Irreversible.",
        required: &["package"],
        optional: &[],
        platforms: ANDROID,
        destructive: true,
        example: "clear_app_data(package='com.example.app')",
    },
    ActionDef {
        name: "reset_app",
        purpose: "Restart an app: terminate, clear data where the platform allows it (Android only), then launch fresh.",
        required: &["package"],
        optional: &[],
        platforms: BOTH,
        destructive: true,
        example: "reset_app(package='com.example.app')",
    },
    ActionDef {
        name: "open_url",
        purpose: "Open an http(s) URL in the system browser.",
        required: &["url"],
        optional: &[],
        platforms: BOTH,
        destructive: true,
        example: "open_url(url='https://example.com')",
    },
    ActionDef {
        name: "deep_link",
        purpose: "Open an app-specific deep link URI (e.g. 'myapp://path').",
        required: &["url"],
        optional: &[],
        platforms: BOTH,
        destructive: true,
        example: "deep_link(url='myapp://settings/account')",
    },

    // --- Device control (non-destructive) ---
    ActionDef {
        name: "set_location",
        purpose: "Set the simulated GPS location (lat/lng). Great for delivery/maps flows.",
        required: &["lat", "lng"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "set_location(lat=29.3759, lng=47.9774)",
    },
    ActionDef {
        name: "clear_location",
        purpose: "Stop simulating GPS location (revert to default).",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "clear_location()",
    },
    ActionDef {
        name: "set_appearance",
        purpose: "Switch the system appearance to dark or light mode.",
        required: &["dark"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "set_appearance(dark=true)",
    },
    ActionDef {
        name: "simulate_biometric",
        purpose: "Answer a Face ID / Touch ID prompt (match=success, nomatch=fail). Sim must be enrolled.",
        required: &["matches"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "simulate_biometric(matches=true)",
    },
    ActionDef {
        name: "pasteboard_set",
        purpose: "Set the device clipboard text (paste OTPs, long strings).",
        required: &["text"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "pasteboard_set(text='123456')",
    },
    ActionDef {
        name: "pasteboard_get",
        purpose: "Read the device clipboard text. Returns it in the result.",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "pasteboard_get()",
    },
    ActionDef {
        name: "grant_permission",
        purpose: "Pre-grant an app a system permission so its consent dialog never appears. iOS service name (location, photos, camera, microphone, contacts, all) or Android android.permission.* string.",
        required: &["permission", "package"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "grant_permission(permission='location', package='com.example.app')",
    },
    ActionDef {
        name: "set_orientation",
        purpose: "Rotate the device: portrait, landscape, landscape_left, landscape_right, portrait_upside_down.",
        required: &["orientation"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "set_orientation(orientation='landscape')",
    },

    ActionDef {
        name: "unlock",
        purpose: "Wake the screen and dismiss the lock screen so the device can be driven.",
        required: &[],
        optional: &[],
        platforms: ANDROID,
        destructive: false,
        example: "unlock()",
    },

    // --- Alerts / dialogs ---
    ActionDef {
        name: "alert_text",
        purpose: "Read the text of the alert or permission dialog on screen. Says so explicitly when nothing is showing, and errors when the device cannot be inspected.",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "alert_text()",
    },
    ActionDef {
        name: "alert_accept",
        purpose: "Tap the positive button of the alert on screen (Allow / OK / Yes). Errors if no alert is showing.",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "alert_accept()",
    },
    ActionDef {
        name: "alert_dismiss",
        purpose: "Tap the negative button of the alert on screen (Deny / Cancel / Not Now). Errors if no alert is showing.",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "alert_dismiss()",
    },
    ActionDef {
        name: "app_state",
        purpose: "Query an app's lifecycle state: not_running, background, or foreground.",
        required: &["package"],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "app_state(package='com.example.app')",
    },

    // --- Recording / observability ---
    ActionDef {
        name: "start_recording",
        purpose: "Start screen recording (saves to device, retrievable later).",
        required: &[],
        optional: &["output_path"],
        platforms: BOTH,
        destructive: false,
        example: "start_recording()",
    },
    ActionDef {
        name: "stop_recording",
        purpose: "Stop the screen recording started by `start_recording`.",
        required: &[],
        optional: &[],
        platforms: BOTH,
        destructive: false,
        example: "stop_recording()",
    },

];

pub fn action_names() -> Vec<&'static str> {
    ACTIONS.iter().map(|a| a.name).collect()
}

pub fn lookup(name: &str) -> Option<&'static ActionDef> {
    ACTIONS.iter().find(|a| a.name == name)
}

/// Names of actions classified as destructive. Used by the hint engine
/// as a deny-list — hints must never suggest any of these.
pub fn destructive_action_names() -> Vec<&'static str> {
    ACTIONS
        .iter()
        .filter(|a| a.destructive)
        .map(|a| a.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_has_non_empty_metadata() {
        for action in ACTIONS {
            assert!(!action.name.is_empty(), "action has empty name");
            assert!(
                !action.purpose.is_empty(),
                "{}: purpose is empty",
                action.name
            );
            assert!(
                !action.example.is_empty(),
                "{}: example is empty",
                action.name
            );
            assert!(
                !action.platforms.is_empty(),
                "{}: platforms is empty",
                action.name
            );
        }
    }

    #[test]
    fn action_names_are_unique() {
        let mut names: Vec<&str> = action_names();
        names.sort();
        let len = names.len();
        names.dedup();
        assert_eq!(len, names.len(), "duplicate action name detected");
    }

    #[test]
    fn destructive_subset_is_known() {
        // Pin the destructive set so tier classification doesn't drift silently.
        let destructive: Vec<&str> = destructive_action_names();
        let expected: Vec<&str> = vec![
            "install",
            "uninstall",
            "terminate_app",
            "clear_app_data",
            "reset_app",
            "open_url",
            "deep_link",
        ];
        let mut d = destructive;
        d.sort();
        let mut e = expected;
        e.sort();
        assert_eq!(
            d, e,
            "destructive action set changed — review hint deny-list"
        );
    }
}
