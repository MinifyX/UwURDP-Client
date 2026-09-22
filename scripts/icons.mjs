// Regenerates the desktop icons from brand/:
//
//   node scripts/icons.mjs
//
// The taskbar, the window, the setup and Linux menus get Nyu without the tile:
// brand/uwurdp-taskbar-icon.svg, upright, on a transparent background. At 16 and 24 px the ICO
// uses brand/uwurdp-taskbar-icon-small.svg instead, with thicker outlines and a flat window,
// which otherwise turn to mush at that size. The macOS icon.icns and the Square*/StoreLogo tiles
// come from the app icon, brand/uwurdp-app-icon.svg, like the website and GitHub.

import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const desktop = join(root, 'apps/desktop');
const icons = join(desktop, 'src-tauri/icons');
const TILE_FILES = [
  'icon.icns',
  'StoreLogo.png',
  ...[30, 44, 71, 89, 107, 142, 150, 284, 310].map((n) => `Square${n}x${n}Logo.png`),
];
const DESKTOP_FILES = [
  'icon.ico',
  'icon.png',
  '32x32.png',
  '64x64.png',
  '128x128.png',
  '128x128@2x.png',
];
/** ICO sizes drawn from the small symbol. */
const SMALL = new Set([16, 24]);

function render(svg) {
  const out = mkdtempSync(join(tmpdir(), 'uwurdp-icons-'));
  execFileSync('pnpm', ['tauri', 'icon', join(root, 'brand', svg), '-o', out], {
    cwd: desktop,
    stdio: 'ignore',
    shell: process.platform === 'win32',
  });
  return out;
}

/** ICO entries by size: { size → image bytes }. A width byte of 0 means 256. */
function readIco(path) {
  const buf = readFileSync(path);
  const entries = new Map();
  for (let i = 0; i < buf.readUInt16LE(4); i++) {
    const at = 6 + i * 16;
    const size = buf[at] || 256;
    entries.set(
      size,
      buf.subarray(buf.readUInt32LE(at + 12), buf.readUInt32LE(at + 12) + buf.readUInt32LE(at + 8)),
    );
  }
  return entries;
}

function writeIco(path, entries) {
  const sizes = [...entries.keys()].sort((a, b) => a - b);
  const header = Buffer.alloc(6 + sizes.length * 16);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(sizes.length, 4);
  let offset = header.length;
  sizes.forEach((size, i) => {
    const at = 6 + i * 16;
    header[at] = header[at + 1] = size % 256;
    header.writeUInt16LE(1, at + 4); // planes
    header.writeUInt16LE(32, at + 6); // bits per pixel
    header.writeUInt32LE(entries.get(size).length, at + 8);
    header.writeUInt32LE(offset, at + 12);
    offset += entries.get(size).length;
  });
  writeFileSync(path, Buffer.concat([header, ...sizes.map((size) => entries.get(size))]));
}

const app = render('uwurdp-app-icon.svg');
const large = render('uwurdp-taskbar-icon.svg');
const small = render('uwurdp-taskbar-icon-small.svg');
try {
  for (const file of TILE_FILES) copyFileSync(join(app, file), join(icons, file));
  for (const file of DESKTOP_FILES) copyFileSync(join(large, file), join(icons, file));
  const ico = readIco(join(large, 'icon.ico'));
  for (const [size, image] of readIco(join(small, 'icon.ico')))
    if (SMALL.has(size)) ico.set(size, image);
  writeIco(join(icons, 'icon.ico'), ico);
} finally {
  for (const dir of [app, large, small]) rmSync(dir, { recursive: true, force: true });
}
