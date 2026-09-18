#!/usr/bin/env bash
# release-local.sh — ship a Drengr release WITHOUT GitHub Actions.
#
# Builds all 4 platform binaries on macOS (Apple natively, Linux via Zig —
# no Docker), packages them exactly like CI, then creates the GitHub release,
# publishes to npm, and registers on the MCP registry. Use this whenever CI is
# unavailable (e.g. Actions billing lapses) or for a fully local release.
#
# ── One-time prerequisites ───────────────────────────────────────────────
#   brew install zig
#   cargo install cargo-zigbuild
#   rustup target add x86_64-apple-darwin \
#                     x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
#   npm: an AUTOMATION token in ~/.npmrc (bypasses 2FA — a login session token
#        will hit an OTP prompt). Set it WITHOUT it touching your shell history:
#        printf "npm token: "; stty -echo; read T; stty echo; echo; \
#          npm config set //registry.npmjs.org/:_authToken "$T"; unset T
#   gh auth login   (must have write access to SharminSirajudeen/drengr-hands)
#   mcp-publisher installed; mcp-registry-key.pem present in the repo root.
#
# ── Usage ────────────────────────────────────────────────────────────────
#   1. bump version (Cargo.toml/npm/package.json/mcpb/server.json), commit, tag:
#        git tag vX.Y.Z && git push origin main vX.Y.Z
#   2. scripts/release-local.sh            # version from Cargo.toml
#      scripts/release-local.sh v0.9.2     # or pass an explicit tag
#
# Notes: builds from the *tag* in a throwaway git worktree (reproducible, never
# touches your working tree). No GPG signing (CI does that with a secret) —
# install.sh treats GPG as optional, so SHA256-only releases install fine. Only
# the public drengr-hands release is made (install.sh + npm both pull there;
# the CI's private "internal record" release is skipped).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RELEASE_REPO="SharminSirajudeen/drengr-hands"
APPLE=(aarch64-apple-darwin x86_64-apple-darwin)
LINUX=(x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu)
# ~/.cargo/bin = rustup arm64 cargo (the system cargo builds x86_64, tripping the
# binary's Rosetta guard). ~/.local/bin = arm64 gh — the system gh at
# /usr/local/bin is x86_64 and intermittently SIGSEGVs in TLS under Rosetta,
# killing the GitHub-release upload after a clean build. Both go ahead of PATH.
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"

VERSION="${1:-v$(grep -m1 '^version' "$REPO_ROOT/Cargo.toml" | cut -d'"' -f2)}"
echo "▶ Releasing $VERSION (local, no CI)"

# ── Preflight ────────────────────────────────────────────────────────────
git -C "$REPO_ROOT" rev-parse "$VERSION" >/dev/null 2>&1 \
  || { echo "✗ tag $VERSION not found — commit + tag the release first"; exit 1; }
command -v zig            >/dev/null || { echo "✗ missing zig — brew install zig"; exit 1; }
command -v cargo-zigbuild >/dev/null || { echo "✗ missing cargo-zigbuild — cargo install cargo-zigbuild"; exit 1; }
command -v mcp-publisher  >/dev/null || { echo "✗ missing mcp-publisher"; exit 1; }
gh auth status >/dev/null 2>&1       || { echo "✗ gh not authenticated — gh auth login"; exit 1; }
# Fail loudly now, not with a Rosetta SIGSEGV mid-upload after an 8-min build.
if [ "$(uname -m)" = "arm64" ] && file "$(command -v gh)" 2>/dev/null | grep -q x86_64; then
  echo "✗ gh is x86_64 (Rosetta) — it faults in TLS during the GitHub upload."
  echo "  Install arm64 gh into ~/.local/bin (this script prepends it to PATH):"
  echo "    L=\$(curl -fsSL https://api.github.com/repos/cli/cli/releases/latest | grep -m1 tag_name | cut -d'\"' -f4); V=\${L#v}"
  echo "    curl -fsSL \"https://github.com/cli/cli/releases/download/\$L/gh_\${V}_macOS_arm64.zip\" -o /tmp/gh.zip"
  echo "    unzip -qo /tmp/gh.zip -d /tmp/ghx && cp /tmp/ghx/*/bin/gh ~/.local/bin/gh"
  exit 1
fi
npm whoami     >/dev/null 2>&1       || { echo "✗ npm not authenticated — set an Automation token in ~/.npmrc"; exit 1; }
[ -f "$REPO_ROOT/mcp-registry-key.pem" ] || { echo "✗ mcp-registry-key.pem missing from repo root"; exit 1; }
# Version-consistency guard: every manifest must match Cargo.toml (VERSION is
# derived from it). Prevents the drift that shipped a stale 0.9.4 lockfile —
# scripts/set-version.sh keeps all sites in sync; this fails the release if not.
if command -v jq >/dev/null; then
  VNUM="${VERSION#v}"
  for chk in \
    "npm/package.json:$(jq -r '.version' "$REPO_ROOT/npm/package.json")" \
    "mcpb/manifest.json:$(jq -r '.version' "$REPO_ROOT/mcpb/manifest.json")" \
    "server.json:$(jq -r '.version' "$REPO_ROOT/server.json")" \
    "server.json/pkg:$(jq -r '.packages[0].version' "$REPO_ROOT/server.json")"; do
    [ "${chk##*:}" = "$VNUM" ] || { echo "✗ version drift: ${chk%%:*} is ${chk##*:}, expected $VNUM — run scripts/set-version.sh $VNUM"; exit 1; }
  done
  echo "✓ version sites consistent at $VNUM"
fi

# ── Clean, reproducible checkout of the tag ──────────────────────────────
WT="$(mktemp -d)/drengr-$VERSION"
git -C "$REPO_ROOT" worktree add -f "$WT" "$VERSION" >/dev/null
trap 'git -C "$REPO_ROOT" worktree remove "$WT" --force 2>/dev/null || true; rm -f /tmp/drengr-verify.tar.gz' EXIT
cd "$WT"
export RUSTFLAGS="--remap-path-prefix=$WT=/drengr --remap-path-prefix=$HOME/.cargo/registry/src=/deps --remap-path-prefix=src/=/m/"

# ── Build (macOS native, Linux via Zig) ──────────────────────────────────
for t in "${APPLE[@]}"; do
  echo "▶ build $t (native)"; rustup target add "$t" >/dev/null 2>&1 || true
  cargo build --release --target "$t"
done
for t in "${LINUX[@]}"; do
  echo "▶ build $t (zigbuild)"; rustup target add "$t" >/dev/null 2>&1 || true
  cargo zigbuild --release --target "$t"
done

echo "▶ iOS helper (per-arch)"

echo "▶ packaging"
for t in "${APPLE[@]}" "${LINUX[@]}"; do
  A="drengr-${VERSION}-${t}.tar.gz"
  ( cd "target/$t/release" \
  shasum -a 256 "$A" > "$A.sha256"
done

# ── GitHub release (public community repo) ───────────────────────────────
echo "▶ GitHub release on $RELEASE_REPO"
NOTES="## Drengr ${VERSION}

### Install
\`\`\`bash
curl -fsSL https://drengr.dev/install.sh | bash
\`\`\`
Or: \`npm install -g drengr\`

### SHA256 Checksums
\`\`\`
$(cat drengr-${VERSION}-*.sha256)
\`\`\`"
gh release delete "$VERSION" --repo "$RELEASE_REPO" --yes 2>/dev/null || true
gh release create "$VERSION" --repo "$RELEASE_REPO" --title "Drengr ${VERSION}" --notes "$NOTES" \
  drengr-${VERSION}-*.tar.gz drengr-${VERSION}-*.sha256

# ── Verify the live asset BEFORE npm (don't publish a broken install path) ─
echo "▶ verifying release download"
curl -fsSL "https://github.com/$RELEASE_REPO/releases/download/$VERSION/drengr-${VERSION}-aarch64-apple-darwin.tar.gz" -o /tmp/drengr-verify.tar.gz
EXP=$(awk '{print $1}' "drengr-${VERSION}-aarch64-apple-darwin.tar.gz.sha256")
ACT=$(shasum -a 256 /tmp/drengr-verify.tar.gz | awk '{print $1}')
[ "$EXP" = "$ACT" ] || { echo "✗ release asset checksum mismatch — aborting before npm"; exit 1; }
echo "✓ release verified"

# ── npm + MCP registry ───────────────────────────────────────────────────
echo "▶ npm publish"
( cd npm && npm publish --access public )

echo "▶ MCP registry publish"
SEED=$(openssl pkey -in "$REPO_ROOT/mcp-registry-key.pem" -outform DER 2>/dev/null | tail -c 32 | xxd -p -c 64)
# The registry auth endpoint occasionally resets the connection mid-handshake
# (transient). npm + the GitHub release are already published by this point, so
# retry a few times rather than fail an otherwise-complete release.
mcp_ok=0
for attempt in 1 2 3; do
  if mcp-publisher login http --domain drengr.dev --private-key "$SEED" >/dev/null 2>&1 \
     && mcp-publisher publish; then
    mcp_ok=1; break
  fi
  echo "  MCP publish attempt $attempt failed (transient?) — retrying in 5s…"
  sleep 5
done
if [ "$mcp_ok" -ne 1 ]; then
  echo "✗ MCP registry publish failed after 3 attempts — but npm + the GitHub"
  echo "  release for $VERSION ARE live. Re-run only the registry step:"
  echo "    SEED=\$(openssl pkey -in mcp-registry-key.pem -outform DER | tail -c 32 | xxd -p -c 64)"
  echo "    mcp-publisher login http --domain drengr.dev --private-key \"\$SEED\" && mcp-publisher publish"
  exit 1
fi

echo "✅ $VERSION fully released — GitHub + npm + MCP registry, no CI used."
