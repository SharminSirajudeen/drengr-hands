# Local release (no GitHub Actions)

Ship a release entirely from a Mac when CI is unavailable (e.g. GitHub Actions
billing lapsed) or when you want a fully local release. One command does the
whole thing: `scripts/release-local.sh`.

## Why this exists
GitHub Actions billing can block the `release.yml` workflow (build jobs refuse
to start). The bottleneck used to be the **Linux binaries** — you can't run
`cross` without Docker. The fix is **`cargo-zigbuild`**, which uses Zig as the
cross-linker to build both Linux arches from macOS with no Docker, and handles
the C crypto deps (`ring`/rustls) that normally break Mac→Linux builds.

## One-time setup (already done on this machine)
```bash
brew install zig
cargo install cargo-zigbuild
rustup target add x86_64-apple-darwin x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu
gh auth login                       # needs write on SharminSirajudeen/drengr-hands
# npm: an AUTOMATION token in ~/.npmrc (NOT a login-session token — that hits a 2FA OTP).
#   Create one at npmjs.com → Access Tokens → Generate → Automation, then set it
#   without leaking it to shell history:
printf "npm token: "; stty -echo; read T; stty echo; echo; npm config set //registry.npmjs.org/:_authToken "$T"; unset T
```
`mcp-registry-key.pem` must be in the repo root (it is).

## Per release
```bash
# 1. Bump the version everywhere, commit:
#    Cargo.toml, npm/package.json, mcpb/manifest.json, server.json  → X.Y.Z
#    (+ CHANGELOG.md)
git commit -am "…"
# 2. Tag and push:
git tag vX.Y.Z && git push origin main vX.Y.Z
# 3. Release (builds 4 platforms, GitHub release, npm, MCP registry):
scripts/release-local.sh           # or: scripts/release-local.sh vX.Y.Z
```

Pushing the tag also triggers the GitHub `release.yml` workflow, which will
**fail on billing — ignore it**; the script does the real release. The script
builds from the *tag* in a throwaway worktree, so your working tree is untouched.

## What the script does
Builds aarch64/x86_64 macOS (native) + x86_64/aarch64 Linux (zigbuild) + the
per-arch iOS helper → packages `drengr-vX.Y.Z-<target>.tar.gz` + `.sha256`
(same names `install.sh`/npm expect) → `gh release create` on
`drengr-hands` → **verifies the live download checksum** → `npm publish` →
`mcp-publisher publish`. No GPG (CI-only secret); `install.sh` treats GPG as
optional, so SHA256-only installs fine.

## Verify after
```bash
curl -s https://registry.npmjs.org/drengr/latest | python3 -c "import json,sys;print(json.load(sys.stdin)['version'])"
npm i -g drengr@latest && drengr --version
```
