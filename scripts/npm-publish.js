// Publishes the npm platform packages, then prp, skipping versions already on npm.
// Usage: node scripts/npm-publish.js [arguments for `yarn npm publish`]
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { executable, packageDir, rootDir, targets } from './npm-package.js';

const mainDir = path.join(rootDir, 'npm/prp');
const read = dir => JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8'));
const { version } = read(mainDir);

const args = process.argv.slice(2);
const prerelease = version.split('-')[1]?.split('.')[0];
if (prerelease != null && !args.includes('--tag')) {
  // A prerelease published without a tag would become `latest`.
  args.push('--tag', /^\d+$/.test(prerelease) ? 'next' : prerelease);
}

for (const target of targets) {
  const dir = packageDir(target);
  const binary = path.join(dir, 'bin', executable(target));
  if (!fs.existsSync(binary)) {
    throw new Error(`${path.relative(rootDir, binary)} is missing; run scripts/npm-package.js first`);
  }
  const { name, version: own } = read(dir);
  if (own !== version) {
    throw new Error(`${name}@${own} does not match prp@${version}`);
  }
}

async function isPublished(name) {
  const response = await fetch(`https://registry.npmjs.org/${name}/${version}`);
  if (response.status === 404) {
    return false;
  }
  if (!response.ok) {
    throw new Error(`Cannot look up ${name}@${version}: ${response.status}`);
  }
  return true;
}

for (const dir of [...targets.map(packageDir), mainDir]) {
  const { name } = read(dir);
  if (await isPublished(name)) {
    console.log(`${name}@${version} is already published`);
    continue;
  }
  console.log(`publish ${name}@${version}`);
  execFileSync('yarn', ['npm', 'publish', ...args], { cwd: dir, stdio: 'inherit' });
}
