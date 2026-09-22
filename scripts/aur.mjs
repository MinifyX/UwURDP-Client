// The Arch Linux package uwurdp-bin on the AUR: writes its PKGBUILD and
// .SRCINFO for a release, from packaging/aur/uwurdp-bin/PKGBUILD.in and the
// release's SHA256SUMS.txt. No dependencies, no makepkg needed.
//
//   node scripts/aur.mjs <version> [--sums SHA256SUMS.txt] [--out <folder>]
//
// Without --sums it reads SHA256SUMS.txt from the published release; without
// --out it writes into target/aur/uwurdp-bin. `pnpm release` calls it after
// publishing and CI's `aur` job (.github/workflows/aur.yml) pushes the result.

import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const REPOSITORY = 'MinifyX/UwURDP-Client';
const DEBS = { x86_64: 'UwURDP-linux-x64.deb', aarch64: 'UwURDP-linux-arm64.deb' };

/**
 * pacman's version for a release: no `-` allowed, and `0.3.0beta.2` sorts
 * before `0.3.0` the way `0.3.0-beta.2` does.
 */
export const pkgver = (version) => version.replaceAll('-', '');

/** `sha256sum` output → file name → hash. */
export function parseSums(text) {
  const sums = new Map();
  for (const line of text.split(/\r?\n/)) {
    const match = /^([0-9a-f]{64}) [ *](.+)$/.exec(line.trim());
    if (match) sums.set(match[2], match[1]);
  }
  return sums;
}

/** `{ PKGBUILD, .SRCINFO }` for a version, given the release's SHA256SUMS.txt text. */
export function aurFiles({ version, sums }) {
  if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/.test(version)) {
    throw new Error(`Unexpected version ${version}`);
  }
  const hashes = parseSums(sums);
  const hash = (arch) => {
    const found = hashes.get(DEBS[arch]);
    if (!found) throw new Error(`SHA256SUMS.txt has no ${DEBS[arch]}.`);
    return found;
  };
  // LF whatever the checkout did to it: a Windows checkout may hand over CRLF.
  const template = readFileSync(join(root, 'packaging/aur/uwurdp-bin/PKGBUILD.in'), 'utf8')
    .split('\r\n')
    .join('\n');
  const PKGBUILD = template
    .replaceAll('@PKGVER@', pkgver(version))
    .replaceAll('@VERSION@', version)
    .replaceAll('@SHA256_X86_64@', hash('x86_64'))
    .replaceAll('@SHA256_AARCH64@', hash('aarch64'));

  // What `makepkg --printsrcinfo` would say about the PKGBUILD above.
  const url = `https://github.com/${REPOSITORY}`;
  const name = 'uwurdp-bin';
  const lines = [
    `pkgbase = ${name}`,
    '\tpkgdesc = Remote desktop client with self-hosted encrypted sync',
    `\tpkgver = ${pkgver(version)}`,
    '\tpkgrel = 1',
    `\turl = ${url}`,
    '\tarch = x86_64',
    '\tarch = aarch64',
    '\tlicense = GPL-3.0-only',
    '\tdepends = webkit2gtk-4.1',
    '\tdepends = gtk3',
    '\tdepends = alsa-lib',
    '\tprovides = uwurdp',
    '\tconflicts = uwurdp',
    '\toptions = !strip',
    '\toptions = !debug',
  ];
  for (const arch of ['x86_64', 'aarch64']) {
    lines.push(
      `\tsource_${arch} = ${name}-${pkgver(version)}-${arch}.deb::${url}/releases/download/v${version}/${DEBS[arch]}`,
      `\tsha256sums_${arch} = ${hash(arch)}`,
    );
  }
  lines.push('', `pkgname = ${name}`, '');
  return { PKGBUILD, '.SRCINFO': lines.join('\n') };
}

/** Writes both files into `dir`. */
export function writeAur(dir, files) {
  mkdirSync(dir, { recursive: true });
  for (const [name, content] of Object.entries(files)) writeFileSync(join(dir, name), content);
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const args = process.argv.slice(2);
  const option = (flag) => {
    const index = args.indexOf(flag);
    return index >= 0 ? args.splice(index, 2)[1] : undefined;
  };
  const sumsFile = option('--sums');
  const out = option('--out') ?? join(root, 'target', 'aur', 'uwurdp-bin');
  const version = args[0]?.replace(/^v/, '');
  if (!version) {
    console.error('Usage: node scripts/aur.mjs <version> [--sums SHA256SUMS.txt] [--out <folder>]');
    process.exit(1);
  }
  let sums;
  if (sumsFile) {
    sums = readFileSync(sumsFile, 'utf8');
  } else {
    const response = await fetch(
      `https://github.com/${REPOSITORY}/releases/download/v${version}/SHA256SUMS.txt`,
    );
    if (!response.ok) {
      console.error(`✗ No SHA256SUMS.txt for v${version} (${response.status}).`);
      process.exit(1);
    }
    sums = await response.text();
  }
  writeAur(out, aurFiles({ version, sums }));
  console.log(`✧ PKGBUILD and .SRCINFO for uwurdp-bin ${pkgver(version)} in ${out}`);
}
