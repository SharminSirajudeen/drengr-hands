#!/usr/bin/env bash
# npm-platforms.sh — prints "<rust-target>:<platform>" for each npm/platforms
# package. The directories are the one list of platforms; both release paths
# read it here rather than keeping their own.
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/../npm/platforms"
for dir in */; do
  platform="${dir%/}"
  case "${platform#*-}" in
    arm64) arch=aarch64 ;;
    x64)   arch=x86_64 ;;
    *) echo "✗ npm/platforms/$platform: unknown cpu" >&2; exit 1 ;;
  esac
  case "${platform%-*}" in
    darwin) system=apple-darwin ;;
    linux)  system=unknown-linux-gnu ;;
    *) echo "✗ npm/platforms/$platform: unknown os" >&2; exit 1 ;;
  esac
  echo "${arch}-${system}:${platform}"
done
