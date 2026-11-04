---
name: drengr-mobile
description: Drive Android and iOS apps on an emulator, simulator or phone through the drengr MCP server - see the screen, tap, type, swipe and launch apps, and check crashes and network calls after every step. Use when the user asks to open, test or click through a mobile app, reproduce a bug on Android or iOS, check what an app shows on a phone, or do anything on an Android emulator or iOS simulator.
license: MIT
compatibility: Needs the drengr MCP server (npx drengr@latest mcp) and Node.js; Android platform-tools (adb) with an emulator or USB device, or Xcode with an iOS simulator runtime.
metadata:
  author: Drengr
  homepage: https://drengr.dev
  source: https://github.com/SharminSirajudeen/drengr-hands
---

# Drive mobile apps with drengr

drengr gives you eyes and hands on an Android or iOS device through three MCP tools:
`drengr_query`, `drengr_look` and `drengr_do`. You decide what to do; drengr observes, acts,
and tells you what each action changed.

## 0. Check the tools are there

If `drengr_query`, `drengr_look` and `drengr_do` are not in your tool list, stop and give the
user this setup. Do not install anything yourself unless they ask you to.

- Claude Code: `claude mcp add drengr -- npx -y drengr@latest mcp`
- Other MCP clients (Cursor, Windsurf, Claude Desktop): add this to the client's MCP config:
  `{"mcpServers": {"drengr": {"command": "npx", "args": ["-y", "drengr@latest", "mcp"]}}}`
- Android needs platform-tools (adb) and an emulator or USB device; iOS needs Xcode with a
  simulator runtime. `npx -y drengr@latest doctor` lists what is missing.
- Use `drengr@latest`: a bare `drengr` makes npx run any older copy already installed.
- The tools appear in a new session after the server is added.

## 1. Get a device and the app list

- `drengr_query(question="setup")` connects to a booted device and returns `device_id`,
  `platform` and the installed apps with their package ids.
- Nothing booted: `drengr_query(question="setup", headless=true)` boots one (Android first;
  add `platform="ios"` for a simulator).
- Launch with the exact package id from that list:
  `drengr_do(action="launch", package="com.example.app")`.

## 2. The loop: observe cheaply, act, check

1. **Observe** with `drengr_look(format="text")`: a numbered element list of about 300
   tokens. Use `format="image"` to judge layout, colour or anything the text leaves out, and
   `format="grid"` when `element_count` is 0 (Flutter, games, webviews, canvas).
2. **Act** with `drengr_do`, using element numbers from the most recent response only. Numbers
   are reassigned whenever the screen changes.
   - Tap: `drengr_do(action="tap", element=4)`
   - Type: `drengr_do(action="type", text="lipstick", element=1)` focuses the field first;
     `action="clear_and_type"` replaces what is there.
   - Submit: `drengr_do(action="key", keycode="enter")`
   - Off-screen element: `drengr_do(action="tap", element_text="Checkout", scroll_to_find=true)`
   - No element tree: look with `format="grid"`, then
     `drengr_do(action="tap", x=0.5, y=0.82)` (fractions of the screen, 0 to 1).
   - Wait: `drengr_do(action="wait", until="element:Order placed")`, or `until="network:idle"`
     or `until="stable"`.

   Pass `format="text"` to `drengr_do` as well. Its response already carries the new screen, so
   a separate `drengr_look` is rarely needed.
3. **Check** the response before the next step:
   - `situation.new_elements` and `situation.disappeared_elements` show whether the screen
     changed the way you intended. Judge navigation by these: `activity_changed` stays false in
     single-activity apps (Jetpack Compose, Flutter, React Native).
   - `situation.stuck: true` means nothing changed. Observe again; do not repeat the action
     blindly.
   - `situation.crash: true`: call `drengr_query(question="crash", package=...)` and
     `drengr_query(question="logcat", filter=...)`.
   - `network_calls` lists the HTTP calls the action caused (URL, method, status, duration). On
     Android they come from the app's own HTTP logs, so an app that does not log shows none. A
     4xx or 5xx usually explains a screen that did not update.

## 3. Close the keyboard before scrolling

With the soft keyboard open, scroll attempts can type stray characters into the focused field
(seen in drengr 0.10.13: "GT" once per scroll). Before a `swipe` or `scroll_to_find`, call
`drengr_query(question="keyboard")`; if `visible` is true, call `drengr_do(action="back")`
first.

## 4. Finish with proof

Stop calling `drengr_do` once the goal is met, then prove it:
`drengr_query(question="assert", conditions='[{"text": "Order placed", "visible": true}]')`
returns `pass` and a result per condition.

Report in this shape:

- **Result:** done, blocked, or bug found
- **Steps:** numbered, one line each: the action, then what changed
- **Evidence:** the assert result, plus any crash, error status or unexpected screen

## Rules

- Run destructive actions (`uninstall`, `clear_app_data`, `reset_app`, `install`) only when the
  user asked for them.
- Type credentials only when the user gave them for this task; prefer test accounts.
- Unsure of an action's parameters: `drengr_query(question="capabilities")` returns the full
  catalog with an example for each. Do not guess.

Two recorded runs, including a tap that did not do what was intended:
[references/examples.md](references/examples.md).
