import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const rootDir = path.join(import.meta.dirname, '..');
export const targets = [
  { platform: 'darwin', arch: 'arm64', rustTarget: 'aarch64-apple-darwin' },
  { platform: 'darwin', arch: 'x64', rustTarget: 'x86_64-apple-darwin' },
  { platform: 'linux', arch: 'arm64', rustTarget: 'aarch64-unknown-linux-musl' },
  { platform: 'linux', arch: 'x64', rustTarget: 'x86_64-unknown-linux-musl' },
  { platform: 'win32', arch: 'arm64', rustTarget: 'aarch64-pc-windows-msvc' },
  { platform: 'win32', arch: 'x64', rustTarget: 'x86_64-pc-windows-msvc' },
];

export function executable(target) {
  return target.platform === 'win32' ? 'prp.exe' : 'prp';
}

export function packageDir(target) {
  return path.join(rootDir, 'npm', `prp-${target.platform}-${target.arch}`);
}

function main() {
  const targetDir = path.resolve(process.argv[2] ?? path.join(rootDir, 'target'));
  const missing = [];
  for (const target of targets) {
    const source = path.join(targetDir, target.rustTarget, 'release', executable(target));
    if (!fs.existsSync(source)) {
      missing.push(target.rustTarget);
      continue;
    }
    const dest = path.join(packageDir(target), 'bin', executable(target));
    fs.mkdirSync(path.dirname(dest), { recursive: true });
    fs.copyFileSync(source, dest);
    fs.chmodSync(dest, 0o755);
    console.log(`${path.relative(rootDir, source)} -> ${path.relative(rootDir, dest)}`);
  }
  if (missing.length === targets.length) {
    console.error(`No binary found under ${targetDir}`);
    process.exit(1);
  }
  for (const rustTarget of missing) {
    console.warn(`warning: no binary for ${rustTarget}`);
  }
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  main();
}
