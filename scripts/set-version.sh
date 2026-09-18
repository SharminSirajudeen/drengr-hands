#!/usr/bin/env bash
# set-version.sh — the ONE place that sets the Drengr version everywhere.
#
# Cargo.toml is canonical; this propagates the new version to every other
# manifest (npm/package.json, server.json ×2, mcpb/manifest.json) and refreshes
# Cargo.lock, so the five sites can never drift. Drift has shipped before — see
# the "chore: sync Cargo.lock to 0.9.4 (lockfile missed the version bump)"
# commit. Bumping is now: `scripts/set-version.sh 0.10.2`.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NEW="${1:?usage: set-version.sh X.Y.Z}"; NEW="${NEW#v}"
[[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "✗ not semver X.Y.Z: $NEW"; exit 1; }
command -v jq >/dev/null || { echo "✗ jq required — brew install jq"; exit 1; }

# Cargo.toml: only the [package] version (the first `version = "..."`).
perl -i -pe 'if (!$d && s/^version = "[^"]*"/version = "'"$NEW"'"/) { $d=1 }' "$ROOT/Cargo.toml"

# JSON manifests via jq — exact paths, so nested versions are never clobbered.
jq_inplace() { local f="$1"; shift; local t; t="$(mktemp)"; jq "$@" "$f" >"$t" && mv "$t" "$f"; }
jq_inplace "$ROOT/npm/package.json"   --arg v "$NEW" '.version=$v'
jq_inplace "$ROOT/mcpb/manifest.json" --arg v "$NEW" '.version=$v'
jq_inplace "$ROOT/server.json"        --arg v "$NEW" '.version=$v | .packages[].version=$v'

# Cargo.lock: refresh the workspace package entry to match Cargo.toml.
( cd "$ROOT" && cargo update -p drengr-hands >/dev/null 2>&1 || true )

# Verify every site agrees — fail loudly if any disagrees.
echo "→ version set to $NEW"
declare -a sites=(
  "Cargo.toml:$(grep -m1 '^version = ' "$ROOT/Cargo.toml" | cut -d'"' -f2)"
  "npm:$(jq -r '.version' "$ROOT/npm/package.json")"
  "mcpb:$(jq -r '.version' "$ROOT/mcpb/manifest.json")"
  "server.json:$(jq -r '.version' "$ROOT/server.json")"
  "server.json/pkg:$(jq -r '.packages[0].version' "$ROOT/server.json")"
  "Cargo.lock:$(awk '/name = "drengr-hands"/{getline; print; exit}' "$ROOT/Cargo.lock" | cut -d'"' -f2)"
)
ok=1
for s in "${sites[@]}"; do
  printf "  %-18s %s\n" "${s%%:*}" "${s##*:}"
  [ "${s##*:}" = "$NEW" ] || { echo "  ✗ ${s%%:*} != $NEW"; ok=0; }
done
[ "$ok" = 1 ] && echo "✓ all version sites consistent at $NEW" || { echo "✗ version drift — fix before tagging"; exit 1; }
