# Property tests

**The question:** does the code do what its input says, for every input?

Every property runs with a fixed seed (`src/properties.rs`), so a failure reproduces on any machine. Each bug below was found by a property on the original code. It was fixed at its cause and pinned by a unit test of the shrunk case. A mutant that puts the old code back turns both the unit test and the property red.

## Properties

| Property | Where | Cases |
|---|---|---|
| `glob_match` equals a brute-force DP matcher (`p` in `[ab*]{0,6}`, `t` in `[ab]{0,8}`) | `expect_network.rs` | 2000 |
| Never panics, 16 functions (see list below) | `properties.rs`, `appium/tests.rs` | 2000 + 20 inputs repeated past 64 KiB, each |
| `parse_bounds("[l,t][r,b]")` returns the same four `i32`s | `properties.rs` | 2000 |
| `parse_adb_devices` returns exactly the ids in state `device`, in order | `properties.rs` | 2000 |
| `extract_attr_ref` reads a value quoted with `'` or `"` | `properties.rs` | 2000 |
| Accepted device ids have no shell chars, no `..`, no leading `-` | `validate.rs` | 2000 (179 accepted) |
| Accepted package names have no shell chars, no `..` | `validate.rs` | 2000 (139 accepted) |
| Accepted predicate tokens have no shell chars | `validate.rs` | 2000 (314 accepted) |
| `sanitize_logcat_filter` output has no shell chars | `validate.rs` | 2000 (889 accepted) |
| The `input text` arguments, run through a line-for-line model of AOSP `sendText`, type exactly the text | `adb.rs` | 2000 |
| Typing round trip on an API 34 emulator (`tests/android_headless_adb.rs`) | device | 50 strings |

These 16 functions are tested for never panicking: `parse_ui_tree_xml`, `parse_current_activity`, `parse_screen_size`, `parse_pm_list_packages`, `parse_logcat_lines`, `parse_adb_devices`, `parse_bounds`, `extract_attr_ref`, `parse_simctl_devices`, `parse_os_log_lines`, `parse_listapps_output`, `parse_page_source`, `parse_okhttp_logcat`, `parse_ios_network_log`, `parse_ooda_decision` and `sanitize_device_text`. The generator mixes arbitrary Unicode with each parser's own markers and with random `i32` bounds and frames.

**Run time** (25 properties, Apple M5 Max, debug build): 0.38 s in parallel and 2.01 s one at a time. The whole lib suite (709 tests) runs in 5.55 s; it ran 674 tests in 5.46 s before. `[profile.dev.package.proptest] opt-level = 3` cut each 2000-case never-panic property from 0.89 s to 0.07 s.

## Bugs

| # | Function | Shrunk counterexample | Cause | Fix | Before → after |
|---|---|---|---|---|---|
| 1 | `glob_match` | `"*b"` vs `"bb"`. Real case: `"*/orders"` vs `https://shop.com/orders/42/orders` | The last segment was matched at its first occurrence | Rewritten from the rule: first segment is the prefix, last segment the suffix of what remains, middle segments found left to right in between | false → true |
| 2 | `Bounds::height` via `parse_ui_tree_xml` | `bounds="[0,-1023765252][1,1123718396]"` | Unchecked `i32` subtraction on device text | Centres exact in `i64`, sizes saturate | overflow panic → `i32::MAX` |
| 3 | `parse_page_source` | `y="1405198982" height="742284666"` | `y + h` in `i32` | `saturating_add` | overflow panic → `i32::MAX` |
| 4 | `parse_logcat_lines` | a line whose byte 18 falls inside a 3-byte character | Byte-index slice `[..18]` | `split_at_checked(18)`: such a line goes to the raw fallback | slice panic → 1 raw entry |
| 5 | `parse_ooda_decision` | `"}0{"` | `rfind('}')` could land before `find('{')` | Look for `}` after `{` | slice panic → `Err` |
| 6 | `parse_adb_devices` | id `0offline`, state `device` | A substring pre-filter on the whole line | Only the state column decides | device dropped → kept |
| 7 | `type_text` (input-text path) | `"50%"`; `"a%sb"` | `sendText` turns only `%s` into a space, so the `%`→`%%` escape was wrong. A literal `%s` can't be sent in one call | Spaces sent as `%s`, `%` sent as itself, text split between calls at each literal `%s` | device: `"50%%"`, `"a% b"` → exact |
| 8 | `extract_attr_ref` | `text='_R=)!r*Z91:J;R"'` (verbatim from the device) | Only `name="…"` was read; uiautomator quotes a value that holds `"` with `'` | Accept either quote | read `""` → exact |
| 9 | `pasteboard_set` (Android) | every string over 16 characters | AOSP `ClipboardService` has no shell command. `cmd clipboard` prints "No shell command implementation." to stderr and exits 0, and adb errors were swallowed, so it reported success and `type_text` pasted an empty clipboard | Fails on any output, reading stderr too. `type_text` then falls back to `input text` | ASCII over 16 chars typed 0/2 → 2/2 |

In release builds, bugs 2 and 3 do not panic: the arithmetic wraps and gives wrong values.

## The two typing hypotheses (Android E2E, API 34 emulator, adb 1.0.41, seed `0x7e577e87`)

- **(a) `%` → `%%`: confirmed.** On run 43758680600, `"50%"` arrived as `"50%%"`, `"a%sb"` as `"a% b"` and `"%Ant~rU{nt"` as `"%%Ant~rU{nt"`. `sendText` handles `%` the same way on `android14-release` and `main`. Fixed as bug 7.
- **(b) shell metacharacters run on the device: refuted on adb 1.0.41.** All 16 ASCII strings arrived exactly, among them `"don't"`, `"a;b|$(c)"` and `"it's 50% off; a&b | $(id) %s ok"`. adbd does run `exec-out` through `sh -c`, but the adb client single-quotes each argument first (`escape_arg`). The code comment that said `exec-out` bypasses the shell was wrong and has been corrected.

| Path (by length) | Class | Before (run 43758680600) | After (run 43760214291) |
|---|---|---|---|
| input (≤ 16) | ASCII | 10/14 | 14/14 |
| paste (> 16) | ASCII | 0/2 | 2/2 |
| both | Arabic and/or emoji | 0/34 | 0/34 |

The path column labels each case by length, as `type_text` decides. Since fix 9, long text also goes through `input text`.

**Still failing: any non-ASCII text, 0/34.** `input text` cannot type it. `KeyCharacterMap.getEvents` is documented to return null when the virtual key map cannot produce the characters, and `sendText` then reads `events.length` on that null (read in the AOSP source; not instrumented on the device). Stock Android 14 has no clipboard shell command (bug 9). Typing non-ASCII needs a different mechanism, which has not been built. The device test stays red and records this defect.

The run before these two (43758034983) also failed the existing smoke test on its first launcher `ui_tree`. It passed on the next two runs, and the typing test now reopens Settings search if it closes (0 reopens in the after run).
