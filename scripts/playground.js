#!/usr/bin/env node

// Copies the webview-bundle fixture into a new git repository with unreleased
// changes, to try `prp prepare` by hand. Usage: node scripts/playground.js [parent-dir]
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const rootDir = path.join(import.meta.dirname, '..');
const parentDir = path.resolve(process.argv[2] ?? os.tmpdir());
const tags = [
  'bridge/0.3.0',
  'cli/0.3.0',
  'config/0.3.0',
  'core/0.3.0',
  'create-webview-bundle/0.2.0',
  'create-wvb/0.2.0',
  'deno/0.3.0',
  'electron/0.3.1',
  'ffi/0.3.0',
  'node/0.3.1',
  'remote-base/0.3.0',
  'remote-local/0.3.0',
  'tauri/0.3.0',
  'testing/0.3.0',
];
const changes = [
  ['packages/core/src/lib.rs', 'pub fn stream() {}\n', 'feat(core): stream bundles from a remote'],
  ['packages/remote/base/index.js', 'export const retry = true;\n', 'fix(remote): retry failed downloads'],
  ['packages/cli/index.js', 'export const legacy = false;\n', 'feat(cli)!: drop the legacy flags'],
  [
    '.prp/changes/streaming.md',
    '---\ntitle: "Stream bundles from a remote"\npackages: core\nbreaking: false\n---\n\nBundles can now be read while they download.\n',
    'docs: describe streaming',
  ],
  ['README.md', 'More docs.\n', 'docs: update the readme'],
];

const run = (cmd, args, cwd) => execFileSync(cmd, args, { cwd, stdio: ['ignore', 'ignore', 'inherit'] });

run('cargo', ['build', '--quiet', '--manifest-path', path.join(rootDir, 'Cargo.toml')]);
fs.mkdirSync(parentDir, { recursive: true });
const dir = fs.mkdtempSync(path.join(parentDir, 'prp-playground-'));
fs.cpSync(path.join(rootDir, 'tests/fixtures/webview-bundle'), dir, { recursive: true });

const git = (...args) => run('git', args, dir);
const commit = message => {
  git('add', '--all');
  git('commit', '--quiet', '--message', message);
};
git('init', '--quiet', '-b', 'main');
git('config', 'user.name', 'prp playground');
git('config', 'user.email', 'playground@example.com');
commit('chore: init');
for (const tag of tags) {
  git('tag', tag);
}
for (const [file, content, message] of changes) {
  const target = path.join(dir, file);
  fs.mkdirSync(path.dirname(target), { recursive: true });
  fs.appendFileSync(target, content);
  commit(message);
}

const prp = path.join(rootDir, 'target/debug/prp');
console.log(`Ready: ${dir}
  cd ${dir}
  ${prp} prepare --skip-pr        # edit, then commit to a local release branch
  ${prp} prepare --dry-run        # edit, then only show what would change
Start over with: git switch main && git branch -D $(git branch --list 'prp/*')`);
