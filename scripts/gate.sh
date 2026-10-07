#!/usr/bin/env bash
# The merge gate, as CI runs it plus the named mutants: build, tests, clippy, fmt, licences, mutants.
# A red step stops the gate; nothing here is optional.
set -uo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
step() { echo "== $1"; shift; "$@" || { echo "GATE RED at: $*"; exit 1; }; }
step build   cargo build --all-targets --locked
step tests   cargo test --locked
step clippy  cargo clippy --all-targets --locked -- -D warnings
step fmt     cargo fmt --check
if command -v cargo-deny >/dev/null; then step licences cargo deny check licenses; else echo "== licences: cargo-deny not installed, CI runs it"; fi
step mutants scripts/mutants.sh
echo "GATE GREEN"
