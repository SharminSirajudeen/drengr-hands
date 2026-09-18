#!/usr/bin/env node

/**
 * Drengr CLI entry point. Resolves the compiled binary from the per-platform
 * package npm installed for this machine.
 *
 * There is deliberately no postinstall step. npm v12 disables lifecycle scripts
 * by default, so a downloader would silently never run and every install would
 * produce a working wrapper with no binary behind it. Platform packages are
 * selected by npm itself from their `os` and `cpu` fields: no network at install
 * time, no scripts, and it works under --ignore-scripts and offline.
 */

'use strict';

const { spawnSync } = require('child_process');
const path = require('path');

const PLATFORM_PACKAGE = `drengr-${process.platform}-${process.arch}`;

function binaryPath() {
  try {
    const pkgJson = require.resolve(`${PLATFORM_PACKAGE}/package.json`);
    return path.join(path.dirname(pkgJson), process.platform === 'win32' ? 'drengr.exe' : 'drengr');
  } catch {
    return null;
  }
}

const bin = binaryPath();
if (!bin) {
  console.error(
    `\nDrengr has no binary for ${process.platform}-${process.arch}.\n\n` +
    `Supported: macOS (arm64, x64) and Linux (arm64, x64).\n` +
    `If you are on one of those, the platform package was skipped at install time.\n` +
    `Reinstall without --no-optional, or with --include=optional:\n\n` +
    `  npm install -g drengr --include=optional\n`
  );
  process.exit(1);
}

const result = spawnSync(bin, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) {
  console.error(`\nFailed to run Drengr: ${result.error.message}\n`);
  process.exit(1);
}
process.exit(result.status === null ? 1 : result.status);
