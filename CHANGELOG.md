# Changelog

All notable changes to **Drengr** — the MCP server that gives AI agents eyes and
hands on mobile devices (Android + iOS).

The format is based on [Keep a Changelog](https://keepachangelog.com/).

## 0.10.6 — 2026-08-01

### Fixed
- **`drengr run` told you less than `drengr test` did.** Both resolve the same
  client, but `run` carried its own older announcement: a debug-formatted
  provider name, no indication whether the model came from `DRENGR_MODEL` or the
  built-in default, and no endpoint line when `DRENGR_BASE_URL` was set. Both
  paths now print the same line from one function.
- **The setup path still flattened its errors.** `drengr test` reported only the
  outer context when the LLM client could not be built — an unresolvable key or
  endpoint said less than it knew, four lines below a sibling that already
  printed the full chain.

## 0.10.5 — 2026-08-01

### Added
- **Every run says which model it will use and why.** Before a suite starts,
  Drengr prints the provider, the model, whether that came from `DRENGR_MODEL`
  or the built-in default, and the endpoint when one is overridden. A failed run
  no longer leaves you guessing what was actually driving the device.
- **`model` and `base-url` inputs on the GitHub Action.** The action could set a
  provider but not a model or an endpoint, so `DRENGR_BASE_URL` was unreachable
  from CI. Point the action at OpenRouter, LiteLLM, vLLM, or your own gateway and
  run any model without waiting on Drengr to add a provider.

### Fixed
- **A blank `DRENGR_MODEL` was used as the model name.** CI runners export empty
  strings for unset inputs, so an unset `model:` would have sent `""` as the
  model — the same trap the key resolver already guarded against, on the path
  that was about to become reachable from the Action.

## 0.10.4 — 2026-07-31

### Fixed
- **Every default model was dead or dying.** Fireworks' Llama 3.3 returns 404,
  `gemini-2.0-flash` shut down 2026-06-01, `claude-sonnet-4-20250514` retired
  2026-06-15, and Groq's `llama-3.3-70b-versatile` is decommissioned 2026-08-16.
  All defaults now point at current models that accept image input, since the
  OODA loop escalates to vision when screen elements are unlabeled.
- **Failures reported their real cause.** A test failure printed
  `Error: LLM call failed` regardless of what actually happened; the underlying
  status and provider message were built and then discarded when the error chain
  was flattened. A dead model now reports its 404 and the provider's own text.
- **`doctor` agreed with reality.** It checked four hardcoded key names and
  missed Groq, Together, and Fireworks, so it reported "not set" for working
  configurations. It now resolves exactly the way `run` and `test` do, and
  reports the provider and model it would use.

### Added
- **`DRENGR_BASE_URL`** — point Drengr at any OpenAI-compatible endpoint
  (OpenRouter, LiteLLM, vLLM, a self-hosted gateway) without a new provider
  variant or a rebuild. `doctor` shows the override when set.
- **`DRENGR_MODEL`** documented — it already worked, it was just undocumented.

## 0.10.3 — 2026-07-31

### Fixed
- **`drengr test` / `drengr ci` auto-detects the suite file again.** The
  auto-detect work (`file` became optional, with `drengr-tests.yml` /
  `drengr-tests.yaml` / `.drengr/tests.yml` candidates) landed after 0.10.2 was
  tagged, so every published build still demanded an explicit `<FILE>` and the
  documented `drengr test --format json` invocation exited 2.
- **npm tarball no longer ships a stale binary.** `files` listed `bin/`, which
  packed whatever local build artifact happened to sit there; 0.10.2 shipped a
  3.9 MB binary from an old build. Narrowed to `bin/drengr.js` — the real
  binary is fetched by `postinstall` for the host platform, as intended.
- **Release artifacts expire after a day.** The release workflow uploaded
  intermediate build tarballs with no retention, which accumulated until the
  Actions storage quota was exhausted and every build job failed at upload.

## 0.10.2 — 2026-06-23

### Added
- **`drengr_look` and `drengr_do` auto-provision a device when none is
  connected** — instead of erroring, Drengr boots an emulator/simulator headless
  (Android first, iOS fallback) and connects it, so the very first observe/act
  "just works" from a clean machine. This removes the most common first-run wall:
  having to hand-boot a device before anything happens.

### Changed
- Telemetry now identifies the AI host correctly: the MCP client name is read
  from the `initialize` handshake (`clientInfo.name`) instead of a few
  environment-variable guesses that missed most hosts (so usage stopped
  attributing to "unknown").
- Failures now record a scrubbed, truncated reason alongside the error category —
  anonymous, with home paths, emails, IP addresses, and ids removed before send.
- Usage rows carry a one-way, machine-wide hash so reinstalls and multiple OS
  user profiles on one machine are no longer miscounted as separate installs.
  The per-install identifier stays anonymous — only the hash is sent, never a
  raw machine id.
- A one-time `activation` signal marks the first successful action of a session,
  so time-to-first-use can be measured rather than just installs.

## 0.10.1 — 2026-06-22

### Added
- `drengr update --check` — report whether a newer version is available without
  upgrading in place (an agent, or you, can check without mutating the install).

### Changed
- Per-step performance telemetry now records the signals needed to actually
  optimize the agent loop: cost (response payload sizes + LLM token counts),
  quality (truncation/`finish_reason` + model served), feature-discovery
  (vision-escalation rate + on-screen element counts), and LLM rate-limit
  headroom. **Anonymous metrics only — numbers, never content** (see Privacy §2.6).

## 0.10.0 — 2026-06-22

### Added
- **Drive a device straight from your shell — no MCP, no restart, no API key.**
  New `drengr look` / `drengr do` / `drengr query` commands expose Drengr's eyes
  and hands over plain CLI: `look` returns numbered elements + an annotated
  screenshot (saved to `~/.drengr/cli/`), `do` taps/types/swipes/launches apps,
  and `query` provisions or inspects devices. They run the same engine as MCP
  mode — the connected agent is the brain — so any tool-calling agent can use
  Drengr without registering an MCP server or restarting its host.
- The no-device paths and `--help` now advertise device **auto-boot**: `drengr
  query setup --headless` starts an emulator/sim if none is running, so agents
  stop hand-booting devices.

### Changed
- Screenshots returned by `look`/`do` are downscaled to 768px before transport —
  ~4× lighter payloads and ~2× cheaper vision tokens, with no change to where
  taps land.
- Recording sessions now finalize cleanly (no more sessions left open), carry a
  keyless per-machine id, and compute a scene rollup; the post-action screenshot
  is persisted, so a session on disk is a reproducible storyboard.

### Fixed
- The element overlay now scales to the screenshot's resolution, fixing numbered
  dots clustering in the top-left corner on Retina / newer simulators.
- The Free-tier screenshot is now correctly encoded (was PNG bytes mislabeled as
  JPEG).

## 0.9.6 — 2026-06-17

### Changed
- Modern MCP handshake: Drengr now negotiates the protocol version (up to
  `2025-11-25`) instead of pinning the year-old `2024-11-05` revision — it echoes
  the client's requested version when supported and otherwise offers its latest,
  so both legacy and current AI clients connect correctly.
- The server now advertises the display metadata modern hosts use to present it:
  a title, description, website, and an embedded icon on the server, plus a human
  display title for each tool ("Observe Mobile Screen", "Act on Mobile Device",
  "Query Mobile Device") — so clients stop showing the raw `drengr_look`/`_do`/
  `_query` ids.
- Tool descriptions and server instructions now state up front that Drengr drives
  a mobile device, not source code, so AI clients running inside coding IDEs stop
  mislabeling the tools as codebase utilities.

## 0.9.5 — 2026-06-16

### Changed
- More reliable version analytics: the client now reports its version in the
  request body (which survives proxies intact) instead of relying on a header an
  intermediary could strip, and the server corrects any previously mis-recorded
  value on the next check-in. No change to what's collected — opt out anytime
  with `DRENGR_TELEMETRY=off`.

## 0.9.4 — 2026-06-16

### Changed
- `drengr onboard` is now idempotent: if Drengr is already wired into an AI
  client, it offers a quick menu (run the demo, wire another tool, re-check,
  reconfigure) instead of re-walking the whole setup.
- Fewer duplicate backend registrations — `mcp`/`demo`/`run` already register
  the machine inline, so the redundant startup write was removed.

## 0.9.3 — 2026-06-16

### Fixed
- Stable machine identity: Drengr now anchors each machine to its OS machine ID
  instead of a mix that included the hostname and a local file. A reinstall, a
  computer rename, or switching networks no longer makes Drengr think it's a
  brand-new machine — so the free tier and per-machine limits behave correctly.
- `drengr uninstall` now reliably forgets a machine server-side even when its
  identity changed since it first ran.

## 0.9.2 — 2026-06-15

### Added
- One-click wiring for four more hosts (no more copy-paste): Cursor and VS Code
  (global config), Claude Code (`~/.claude.json`, preserving your other state),
  and Xcode 26.3+ (when its agent is set up — with the absolute binary path its
  restricted shell needs, plus a verify note).

### Fixed
- `drengr uninstall` now stops any running Drengr MCP server *before* forgetting
  the machine, so a background heartbeat can't re-register it mid-uninstall.
- Config writes are now atomic (temp + rename) — a running editor can never see
  a half-written config file.

### Changed
- Documented that machine identity (always-on, for the free tier) and anonymous
  bug/usage telemetry (`DRENGR_TELEMETRY=off`) are separate — opting out of
  telemetry was never meant to disable identity.

## 0.9.1 — 2026-06-14

### Fixed
- `drengr setup` / onboard now recover if an MCP client's config file has a
  malformed `mcpServers` value instead of silently dropping the drengr entry.
- Onboarding telemetry now reports where users drop off, not just completion.
- iOS runner wording corrected: it's provisioned once per iOS version + Xcode
  and reused across simulators (a new iOS version provisions itself on first
  use) — not a per-simulator build.

## 0.9.0 — 2026-06-12

### Added
- `drengr onboard` — one guided command from "just installed" to "wired into
  my AI tools, having watched it work": detects your environment, boots a
  device, builds the iOS runner, optionally sets up a local/cloud model, wires
  Drengr into every installed AI client at once, and runs the demo in-flow.
  Auto-launches after install when you're in a real terminal.
- Two more AI clients for `drengr setup` (and the wizard): Antigravity (stdio)
  and Xcode 26.3+ Coding Assistant.
- Clean model setup: detects Ollama, offers to pull the local vision model
  with live progress, or captures a cloud key — and vision-checks it before
  saving so you can't end up with a model that can't see the screen.

### Changed
- Every command now registers this machine with our backend (anonymous, no
  sign-in) so usage and reliability data is complete — unchanged opt-out via
  `DRENGR_TELEMETRY=off`.

## 0.8.2 — 2026-06-11

### Added
- `drengr mcp --http` — serve MCP over streamable HTTP (127.0.0.1, endpoint
  `/mcp`) for clients that can't launch stdio servers. Android Studio's
  Gemini agent mode is the first-class target.
- `drengr setup --client android-studio --write` — writes the `httpUrl`
  config straight into Android Studio's `mcp.json`.

## 0.8.1 — 2026-06-10

### Added
- `drengr demo` boots a simulator/emulator by itself when no device is
  running — one command from a clean machine to a live agent.
- First-run welcome inside your AI client: on a fresh machine the agent offers
  a 30-second live demo before anything else.
- `drengr uninstall` now also asks our server to forget this machine: its
  registration is erased and its usage records are permanently anonymized.
  Keychain entries (license + LLM keys) are removed too. Accounts are never
  deleted by uninstall — that stays an explicit request.
- Machine records auto-purge server-side: keyless registrations after 90 days
  of inactivity, machine activations 12 months after the last heartbeat.

### Fixed
- The MCP server no longer asks keyless users for an API key — v0.8.0's
  "no login required" now applies to MCP tool calls, not just the CLI.
- Browser sign-in (`drengr login`) now completes for users who weren't
  already signed in at drengr.dev.

## 0.8.0 — 2026-06-08

### Added
- `drengr demo` — a one-command demo: auto-detects a connected device (Android
  emulator or iOS simulator) and an AI agent drives a system app, live.
- Browser sign-in: `drengr login` with no key opens your browser and captures
  the key automatically — no copy-paste.

### Changed
- No login required to start. The local CLI and MCP server are free to use with
  no account; sign-in is optional.

## 0.7.14 — 2026-06-05

### Fixed
- A simulator left locked by a previous, abandoned session is now recovered
  automatically, with no manual intervention required.

## 0.7.13 — 2026-06-05

### Fixed
- The on-device test runner self-heals if it stops responding mid-session,
  re-establishing control instead of failing every subsequent action.
- The runner is now released promptly when the host application exits
  unexpectedly, preventing it from blocking the next session.

## 0.7.12 — 2026-06-05

### Fixed
- When another Drengr instance is already driving a simulator, the error is now
  clear and actionable rather than a silent cooldown.

## 0.7.10 — 2026-06-05

### Added
- Per-step performance timing, so slow steps can be measured and optimized.

## 0.7.9 — 2026-06-05

### Added
- iOS reads the foreground application's accessibility tree, restoring
  element-based actions on native apps.

### Changed
- Screenshots are roughly 5× smaller (downscaled JPEG) for faster transfers and
  lower token cost, with no loss of fidelity for the agent.
- Path drawing is smoother and more reliable across long gestures.

## 0.7.8 — 2026-06-05

### Added
- Coordinate tap, swipe, and long-press — control any screen by vision and
  pixel-touch, including framework-less interfaces (Flutter, games, in-app web,
  and HTML canvas) that expose no accessibility tree. A coordinate-grid overlay
  (`drengr_look(format='grid')`) helps the agent aim precisely.

## 0.7.7 — 2026-06-05

### Added
- Android: optional windowed emulator mode (`show_window`) to watch automation
  run live.

## 0.7.6 — 2026-06-05

### Added
- `set_orientation` to rotate the iOS device between portrait and landscape.

## 0.7.5 — 2026-06-05

### Added
- Reliable text input and Spotlight search on iOS.

### Fixed
- The runner survives transient failures instead of ending the session.

## 0.7.4 — 2026-06-04

### Added
- `grant_permission` to pre-authorize system permissions, so consent dialogs
  never interrupt an automated run.

## 0.7.3 — 2026-06-04

### Changed
- All device actions now perform real operations; fixed tapping of visible
  elements.

## 0.7.2 — 2026-06-03

### Added
- Expanded `drengr_do` action coverage and deeper scroll-to-find.

## 0.7.0 — 2026-06-03

### Added
- Granular driver diagnostics with precise error codes.

### Fixed
- Automatic cleanup of stale ("zombie") runners.

## 0.6.0 — 2026-05-26

### Changed
- New iOS driver running fully headless inside CoreSimulator via XCTest,
  replacing the previous WebDriverAgent-based approach.

## 0.4.1 — 2026-05-12

### Fixed
- Resolved an iOS crash loop and improved error capture.

## 0.2.1 — 2026-04-02

### Added
- Full iOS gesture support: tap, swipe, pinch-zoom, and long-press.

## 0.1.8 — 2026-03-19

### Added
- Ten additional MCP tools, binary hardening, and automatic updates.

## 0.1.5 — 2026-03-15

### Added
- Getting-started guide, an uninstall command, and automatic path detection.

## 0.1.1 — 2026-03-15

### Added
- Public landing page, documentation, legal pages, and GPG-signed release
  artifacts.

## 0.1.0 — 2026-03-12

### Added
- Initial public release: an MCP server giving AI agents eyes and hands on
  Android and iOS — screenshots, UI inspection, and actions (tap, type, swipe,
  and more) over ADB and `simctl`.
