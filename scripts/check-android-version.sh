#!/usr/bin/env bash
# Fails when the Android SDK sources have moved since the last version bump.
#
# VersionTest catches Drengr.VERSION disagreeing with the maven coordinates. It
# cannot catch the case that actually bit us: coordinates say 0.3.0, Central
# already serves 0.3.0, and the sources have changed since. Central is immutable,
# so those changes can never reach a consumer under that number. A redactHeaders
# privacy fix sat unpublished this way from 2026-08-31.
set -euo pipefail

SRC="sdk/android/drengr/src/main"
BUILD="sdk/android/drengr/build.gradle.kts"

bump=$(git log -1 --format=%H -G'coordinates\(' -- "$BUILD")
[ -n "$bump" ] || { echo "no commit ever set the coordinates"; exit 1; }

changed=$(git diff --name-only "$bump" HEAD -- "$SRC" | wc -l | tr -d ' ')
# the version as of the bump commit, which is the one Central actually serves
published=$(git show "$bump:$BUILD" | grep -oE 'coordinates\([^)]*"([^"]+)"\s*\)' | grep -oE '"[0-9][^"]*"' | tail -1 | tr -d '"')

if [ "$changed" -gt 0 ]; then
  echo "Android sources changed in $changed file(s) since version $published was declared."
  echo "Maven Central is immutable, so a consumer resolving $published will never get them."
  git diff --name-only "$bump" HEAD -- "$SRC" | sed 's/^/  /'
  echo "Bump coordinates(...) and Drengr.VERSION, then publish."
  exit 1
fi
echo "Android sources are in sync with declared version $published."
