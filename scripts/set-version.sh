#!/usr/bin/env bash
# set-version.sh — the ONE place that sets and checks the Drengr version.
#
#   scripts/set-version.sh X.Y.Z                  set every site, then check
#   scripts/set-version.sh --check X.Y.Z [ROOT]   check only; the release paths
#                                                 run this on what they publish
#
# Cargo.toml is canonical. The sites are Cargo.toml, Cargo.lock, npm/package.json
# and its optionalDependencies (the platform packages that carry the binary),
# every npm/platforms package, mcpb/manifest.json and server.json twice. Drift
# has shipped before: a stale 0.9.4 lockfile, and 0.11.0 pinning the 0.10.13
# binaries.
set -euo pipefail

CHECK_ONLY=0
if [ "${1:-}" = "--check" ]; then CHECK_ONLY=1; shift; fi
NEW="${1:?usage: set-version.sh [--check] X.Y.Z [ROOT]}"; NEW="${NEW#v}"
ROOT="${2:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
[[ "$NEW" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "✗ not semver X.Y.Z: $NEW"; exit 1; }
command -v jq >/dev/null || { echo "✗ jq required — brew install jq"; exit 1; }

jq_inplace() { local f="$1"; shift; local t; t="$(mktemp)"; jq "$@" "$f" >"$t" && mv "$t" "$f"; }

if [ "$CHECK_ONLY" = 0 ]; then
  # Cargo.toml: only the [package] version (the first `version = "..."`).
  perl -i -pe 'if (!$d && s/^version = "[^"]*"/version = "'"$NEW"'"/) { $d=1 }' "$ROOT/Cargo.toml"
  # JSON manifests via jq: exact paths, so nested versions are never clobbered.
  jq_inplace "$ROOT/npm/package.json"   --arg v "$NEW" '.version=$v | .optionalDependencies |= map_values($v)'
  for p in "$ROOT"/npm/platforms/*/package.json; do jq_inplace "$p" --arg v "$NEW" '.version=$v'; done
  jq_inplace "$ROOT/mcpb/manifest.json" --arg v "$NEW" '.version=$v'
  jq_inplace "$ROOT/server.json"        --arg v "$NEW" '.version=$v | .packages[].version=$v'
  # Cargo.lock: refresh the workspace package entry to match Cargo.toml.
  ( cd "$ROOT" && cargo update -p drengr-hands >/dev/null 2>&1 || true )
  echo "→ version set to $NEW"
fi

# The pins and the platform folders must name the same packages: a pin with no
# folder publishes nothing behind it, a folder with no pin is never installed.
folders="$(cd "$ROOT/npm/platforms" && for d in */; do echo "drengr-${d%/}"; done | sort | paste -sd, -)"
pins="$(jq -r '.optionalDependencies | keys | sort | join(",")' "$ROOT/npm/package.json")"
[ "$folders" = "$pins" ] || { echo "✗ npm/platforms has $folders but npm/package.json pins $pins"; exit 1; }

declare -a sites=(
  "Cargo.toml:$(grep -m1 '^version = ' "$ROOT/Cargo.toml" | cut -d'"' -f2)"
  "npm:$(jq -r '.version' "$ROOT/npm/package.json")"
  "npm/optionalDependencies:$(jq -r '[.optionalDependencies[]] | unique | join(",")' "$ROOT/npm/package.json")"
  "npm/platforms:$(jq -rs 'map(.version) | unique | join(",")' "$ROOT"/npm/platforms/*/package.json)"
  "mcpb:$(jq -r '.version' "$ROOT/mcpb/manifest.json")"
  "server.json:$(jq -r '.version' "$ROOT/server.json")"
  "server.json/pkg:$(jq -r '.packages[0].version' "$ROOT/server.json")"
  "Cargo.lock:$(awk '/name = "drengr-hands"/{getline; print; exit}' "$ROOT/Cargo.lock" | cut -d'"' -f2)"
)
ok=1
for s in "${sites[@]}"; do
  printf "  %-26s %s\n" "${s%%:*}" "${s##*:}"
  [ "${s##*:}" = "$NEW" ] || { echo "  ✗ ${s%%:*} != $NEW"; ok=0; }
done
[ "$ok" = 1 ] || { echo "✗ version drift — run scripts/set-version.sh $NEW"; exit 1; }
echo "✓ all version sites consistent at $NEW"
