#!/usr/bin/env node
'use strict';

const { spawnSync } = require('node:child_process');
const os = require('node:os');
const path = require('node:path');

const { version } = require('../package.json');

function binaryPath() {
  if (process.env.PRP_BINARY) {
    return process.env.PRP_BINARY;
  }
  const platform = `prp-${process.platform}-${process.arch}`;
  let manifestPath;
  try {
    manifestPath = require.resolve(`${platform}/package.json`);
  } catch {
    throw new Error(
      `${platform} is not installed. prp has no binary for this platform, or optional dependencies were skipped during install.`
    );
  }
  const manifest = require(manifestPath);
  if (manifest.version !== version) {
    throw new Error(`${platform}@${manifest.version} does not match prp@${version}; reinstall prp.`);
  }
  const executable = process.platform === 'win32' ? 'prp.exe' : 'prp';
  return path.join(path.dirname(manifestPath), 'bin', executable);
}

let binary;
try {
  binary = binaryPath();
} catch (error) {
  console.error(`prp: ${error.message}`);
  process.exit(1);
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' });
if (result.error) {
  console.error(`prp: failed to run ${binary}: ${result.error.message}`);
  process.exit(1);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
  process.exit(128 + (os.constants.signals[result.signal] ?? 0));
}
process.exit(result.status ?? 1);
