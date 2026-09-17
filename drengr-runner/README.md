# Drengr Runner

The tiny XCTest target that hosts Drengr's iOS control plane inside a CoreSimulator instance.

## What it is

- A single Swift file (`DrengrRunner/DrengrRunner.swift`).
- Hosts an HTTP/1.1 server (NWListener) inside a UI Test target.
- Exposes two functional endpoints to the host-side Drengr binary:
  - `GET /observe` — screenshot (PNG) + best-effort UI tree hint
  - `POST /act` — pixel-coordinate touch / type / hardware button
- Plus `GET /status` for health.
- Bundle id: `dev.drengr.runner`. Display name: "Drengr Runner".

The architectural rule: **a touch is a touch is a touch.** See
`/runbooks/v060-drengr-runner-2026-05-14.md` §15 and
`feedback_drengr_touch_is_a_touch.md` (memory).

## Building (one-time setup)

The Drengr binary's `bootstrap.rs` invokes `xcodebuild build-for-testing`
on this project at runtime — users never run this manually after install.
For development on this target, open `DrengrRunner.xcodeproj` in Xcode
or build via CLI:

```sh
xcodebuild build-for-testing \
  -project drengr-runner/DrengrRunner.xcodeproj \
  -scheme DrengrRunner \
  -destination "id=<udid>" \
  -derivedDataPath /tmp/drengr-runner-build \
  -configuration Release \
  CODE_SIGNING_ALLOWED=NO \
  CODE_SIGN_IDENTITY="" \
  CODE_SIGN_REQUIRED=NO
```

The resulting `DrengrRunner-Runner.app` is at
`/tmp/drengr-runner-build/Build/Products/Release-iphonesimulator/`.

## Manual smoke test

After building, install and launch into a booted sim:

```sh
xcrun simctl install <udid> /tmp/drengr-runner-build/Build/Products/Release-iphonesimulator/DrengrRunner-Runner.app
xcrun simctl launch --terminate-running-process <udid> dev.drengr.runner --env DRENGR_RUNNER_PORT=8200
curl http://localhost:8200/status | jq .
```

Expected output:

```json
{
  "ok": true,
  "product": "drengr-runner",
  "version": "0.6.0",
  "ios_major": 19,
  "screen": { "width": 393, "height": 852, "scale": 3.0 }
}
```

## Xcode project setup

The `.xcodeproj` is generated deterministically from `project.yml` using
[XcodeGen](https://github.com/yonaskolb/XcodeGen). The YAML is the source
of truth; the generated `.xcodeproj/project.pbxproj` is committed
alongside it as a build artifact so end users don't need XcodeGen.

### To regenerate (run when `project.yml` changes)

```sh
brew install xcodegen      # one-time install
cd drengr-runner
xcodegen generate
git add DrengrRunner.xcodeproj
```

`USES_XCTRUNNER=YES` in `project.yml` tells Xcode to auto-generate the
`DrengrRunner-Runner.app` xctrunner host that wraps the `.xctest` bundle —
this is the standalone "no host app" XCUITest pattern WDA and Maestro
use.

## Status

- Phase 1: HTTP server + `/status` ← **current**
- Phase 2: `/observe` (screenshot + tree hint)
- Phase 3: `/act` (tap / swipe / draw_path / type / button)
- See `/runbooks/v060-drengr-runner-2026-05-14.md` for the full plan.
