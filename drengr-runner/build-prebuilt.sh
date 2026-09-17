#!/usr/bin/env bash
# Rebuilds the drengr-runner XCTest target and embeds it into prebuilt/ with the
# Drengr app icon + display name on the xctrunner host app.
#
# The committed prebuilt is what ships embedded in the drengr binary — end users
# never run this; only run it after changing drengr-runner/ sources or the icon.
#
# Icon note: actool can only THIN an app icon against an installed simulator
# runtime that matches the Xcode SDK. Rather than force a multi-GB runtime
# download just to recompile an unchanged icon, we build icon-less and graft the
# already-committed branded Assets.car. To re-render the icon ART itself, install
# a runtime matching the SDK and pass REBUILD_ICON=1.
set -euo pipefail
cd "$(dirname "$0")/.."   # repo root
PROJ="drengr-runner/DrengrRunner.xcodeproj"
DERIVED="/tmp/drengr-runner-build"
DST="drengr-runner/prebuilt/Release-iphonesimulator/DrengrRunner-Runner.app"

# Stash the committed branded icon up front (source of truth when not rebuilding it).
ICON_STASH="$(mktemp -d)/Assets.car"
[ -f "$DST/Assets.car" ] && cp "$DST/Assets.car" "$ICON_STASH"

# xcodebuild's destination resolver is flaky with the generic placeholder; a
# concrete booted-sim UDID is reliable. Pick a booted iOS sim, else boot one.
UDID="$(xcrun simctl list devices booted | grep -oE '[0-9A-F-]{36}' | head -1 || true)"
if [ -z "$UDID" ]; then
  UDID="$(xcrun simctl list devices available | grep -iE 'iphone' | grep -oE '[0-9A-F-]{36}' | head -1 || true)"
  [ -n "$UDID" ] || { echo "ERROR: no available iOS simulator to build against" >&2; exit 1; }
  xcrun simctl boot "$UDID" 2>/dev/null || true
fi
xcrun simctl bootstatus "$UDID" -b >/dev/null 2>&1 || true

ICON_ARGS=(ASSETCATALOG_COMPILER_APPICON_NAME="")   # icon-less by default (no runtime needed)
[ "${REBUILD_ICON:-0}" = "1" ] && ICON_ARGS=()

build_ok=""
for attempt in 1 2 3; do
  rm -rf "$DERIVED"
  if xcodebuild build-for-testing \
       -project "$PROJ" -scheme DrengrRunner \
       -destination "platform=iOS Simulator,id=$UDID" \
       -derivedDataPath "$DERIVED" -configuration Release \
       CODE_SIGNING_ALLOWED=NO "${ICON_ARGS[@]}" 2>&1 | grep -q "BUILD SUCCEEDED"; then
    build_ok=1; break
  fi
  echo "build attempt $attempt failed; retrying..." >&2
  sleep 4
done
[ -n "$build_ok" ] || { echo "ERROR: runner build failed after retries" >&2; exit 1; }

APP="$DERIVED/Build/Products/Release-iphonesimulator/DrengrRunner-Runner.app"

# The springboard reads the icon from the xctrunner HOST app. Place the branded
# Assets.car at the host root + register the icon keys and branded display name.
if [ "${REBUILD_ICON:-0}" = "1" ]; then
  cp "$APP/PlugIns/DrengrRunner.xctest/Assets.car" "$APP/Assets.car"
elif [ -f "$ICON_STASH" ]; then
  cp "$ICON_STASH" "$APP/Assets.car"
else
  echo "ERROR: no committed icon to reuse; run once with REBUILD_ICON=1" >&2; exit 1
fi
plutil -replace CFBundleIconName -string AppIcon "$APP/Info.plist" 2>/dev/null \
  || plutil -insert CFBundleIconName -string AppIcon "$APP/Info.plist"
plutil -remove CFBundleIcons "$APP/Info.plist" 2>/dev/null || true
plutil -insert CFBundleIcons -xml \
  '<dict><key>CFBundlePrimaryIcon</key><dict><key>CFBundleIconName</key><string>AppIcon</string></dict></dict>' \
  "$APP/Info.plist"
plutil -replace CFBundleDisplayName -string "Drengr" "$APP/Info.plist"

rm -rf "$DST"
cp -R "$APP" "$DST"
rm -rf "$DST/PlugIns/DrengrRunner.xctest.dSYM"   # don't embed debug symbols
echo "OK: branded runner embedded into $DST"
