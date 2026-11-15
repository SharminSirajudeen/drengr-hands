//! Property tests for the pure parsers. Every runner uses a fixed seed, so a
//! failure reproduces on any machine.

use proptest::prelude::*;
use proptest::test_runner::{Config, RngSeed};

pub(crate) const SEED: u64 = 0x5EED_D2E6;

pub(crate) fn config(cases: u32) -> Config {
    Config {
        cases,
        rng_seed: RngSeed::Fixed(SEED),
        failure_persistence: None,
        ..Config::default()
    }
}

/// Fragments the parsers look for, so generated input reaches past their
/// first `if`. Mixed with arbitrary Unicode by `noisy`.
const MARKERS: &[&str] = &[
    "<node ",
    "bounds=\"",
    "\"",
    "[",
    "]",
    ",",
    "text=\"",
    "class=\"",
    "content-desc=\"",
    "x=\"",
    "y=\"",
    "width=\"",
    "height=\"",
    "<",
    "/>",
    "&#10;",
    "&amp;",
    "EditText",
    "2147483647",
    "-2147483648",
    "99999999999",
    "0",
    "-1",
    "mCurrentFocus",
    "mFocusedApp",
    "u0 ",
    "}",
    "Physical size: ",
    "Override size: ",
    "x",
    "package:",
    "03-18 10:30:45.123  1234  5678 I Tag : ",
    "---",
    "I ",
    ": ",
    "List of devices attached",
    "\tdevice",
    " device ",
    "model:",
    "offline",
    "{\"devices\":{\"r\":[{\"state\":\"Booted\",\"udid\":\"",
    "\",\"name\":\"",
    "\"}]}}",
    "Timestamp",
    "2024-01-01 10:00:00.000 0x1 Default 0x0 ",
    "CFBundle",
    "<key>CFBundleIdentifier</key>",
    "<string>",
    "</string>",
    "CFBundleIdentifier = ",
    "};",
    ";",
    "okhttp.OkHttpClient: ",
    "CURL_TAG: ",
    "--> ",
    "--> END",
    "<-- ",
    "<-- END HTTP",
    "(",
    "ms)",
    "-byte",
    "200 ",
    "GET ",
    "received response",
    "status ",
    "```json",
    "```",
    "{",
    "\"action\": \"tap\"",
    "é",
    "مرحبا",
    "😀",
    "\u{200f}",
    "\u{0}",
    "\n",
    "\r\n",
    " ",
    "\t",
];

fn noisy() -> impl Strategy<Value = String> {
    let piece = prop_oneof![
        1 => proptest::collection::vec(any::<char>(), 0..8).prop_map(String::from_iter),
        3 => proptest::sample::select(MARKERS).prop_map(String::from),
        1 => any::<[i32; 4]>().prop_map(|[l, t, r, b]| format!("<node bounds=\"[{l},{t}][{r},{b}]\" ")),
        1 => any::<[i32; 4]>().prop_map(|[x, y, w, h]| {
            format!("\n<e class=\"c\" x=\"{x}\" y=\"{y}\" width=\"{w}\" height=\"{h}\"/>\n")
        }),
    ];
    proptest::collection::vec(piece, 0..48).prop_map(|v| v.concat())
}

/// `f` returns for 2000 generated inputs and for 20 more repeated past 64 KiB.
pub(crate) fn never_panics(f: impl Fn(&str)) {
    proptest::test_runner::TestRunner::new(config(2000))
        .run(&noisy(), |s| {
            f(&s);
            Ok(())
        })
        .unwrap();
    proptest::test_runner::TestRunner::new(config(20))
        .run(&noisy(), |s| {
            f(&s.repeat(65_536 / (s.len() + 1) + 1));
            Ok(())
        })
        .unwrap();
}

mod never_panic {
    use super::never_panics;
    use crate::network::logcat::{parse_ios_network_log, parse_okhttp_logcat};
    use crate::transport::{adb, detect, extract_attr_ref, parse_bounds, simctl};

    #[test]
    fn parse_ui_tree_xml() {
        never_panics(|s| {
            let _ = adb::parse_ui_tree_xml(s);
        });
    }
    #[test]
    fn parse_current_activity() {
        never_panics(|s| {
            let _ = adb::parse_current_activity(s);
        });
    }
    #[test]
    fn parse_screen_size() {
        never_panics(|s| {
            let _ = adb::parse_screen_size(s);
        });
    }
    #[test]
    fn parse_pm_list_packages() {
        never_panics(|s| {
            let _ = adb::parse_pm_list_packages(s);
        });
    }
    #[test]
    fn parse_logcat_lines() {
        never_panics(|s| {
            let _ = adb::parse_logcat_lines(s);
        });
    }
    #[test]
    fn parse_adb_devices() {
        never_panics(|s| {
            let _ = detect::parse_adb_devices(s);
        });
    }
    #[test]
    fn parse_bounds_any() {
        never_panics(|s| {
            let _ = parse_bounds(s);
        });
    }
    #[test]
    fn extract_attr_ref_any() {
        never_panics(|s| {
            let _ = extract_attr_ref(s, "text");
        });
    }
    #[test]
    fn parse_simctl_devices() {
        never_panics(|s| {
            let _ = simctl::parse_simctl_devices(s);
        });
    }
    #[test]
    fn parse_os_log_lines() {
        never_panics(|s| {
            let _ = simctl::parse_os_log_lines(s);
        });
    }
    #[test]
    fn parse_listapps_output() {
        never_panics(|s| {
            let _ = simctl::parse_listapps_output(s);
        });
    }
    #[test]
    fn parse_okhttp_logcat_any() {
        never_panics(|s| {
            let _ = parse_okhttp_logcat(s);
        });
    }
    #[test]
    fn parse_ios_network_log_any() {
        never_panics(|s| {
            let _ = parse_ios_network_log(s);
        });
    }
    #[test]
    fn parse_ooda_decision() {
        never_panics(|s| {
            let _ = crate::ooda::prompt::parse_ooda_decision(s);
        });
    }
    #[test]
    fn sanitize_device_text() {
        never_panics(|s| {
            let _ = crate::screen::ui_element::sanitize_device_text(s);
        });
    }
}

mod round_trip {
    use super::config;
    use crate::screen::ui_element::Bounds;
    use crate::transport::{detect::parse_adb_devices, parse_bounds};
    use proptest::prelude::*;

    const STATES: &[&str] = &[
        "device",
        "offline",
        "unauthorized",
        "recovery",
        "bootloader",
    ];

    /// One `adb devices -l` row: id, state, and whether it carries a `model:`.
    fn row() -> impl Strategy<Value = (String, String, Option<String>)> {
        let word = proptest::sample::select(&["", "device", "offline", "model"][..]);
        (
            ("[a-zA-Z0-9.:_-]{1,8}", word.clone()).prop_map(|(a, w)| a + w),
            proptest::sample::select(STATES).prop_map(String::from),
            proptest::option::of(("[a-zA-Z0-9_]{1,8}", word).prop_map(|(a, w)| a + w)),
        )
    }

    proptest! {
        #![proptest_config(config(2000))]

        #[test]
        fn parse_bounds_returns_the_four_numbers(l: i32, t: i32, r: i32, b: i32) {
            prop_assert_eq!(parse_bounds(&format!("[{l},{t}][{r},{b}]")), Some(Bounds::new(l, t, r, b)));
        }

        #[test]
        fn parse_adb_devices_returns_every_ready_id_in_order(rows in proptest::collection::vec(row(), 0..8)) {
            let mut out = String::from("List of devices attached\n");
            for (id, state, model) in &rows {
                out += &format!("{id}\t{state}");
                if let Some(m) = model {
                    out += &format!(" product:p model:{m} device:d transport_id:1");
                }
                out.push('\n');
            }
            let want: Vec<&str> = rows.iter().filter(|r| r.1 == "device").map(|r| r.0.as_str()).collect();
            let got: Vec<String> = parse_adb_devices(&out).into_iter().map(|d| d.id).collect();
            prop_assert_eq!(got, want);
        }
    }
}
