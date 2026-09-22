// Builds UwURDP's downloads for the system it runs on: on Windows and macOS
// the app packed into the setup with Nyu in it, on Linux the app as .deb,
// .rpm and a portable folder.
//
//   pnpm build:setup                       for this machine
//   pnpm build:setup --target <triple>     another architecture (macOS:
//                                          universal-apple-darwin for both
//                                          Intel and Apple silicon, as the
//                                          release has it)
//
// Everything lands in target/installers/, under the names the release
// publishes — no version in them, so a link to the newest release's file
// stays the same forever:
//
//   Windows  UwURDP-windows-x64-setup.exe           what people run, and what updates run
//            UwURDP-windows-arm64-setup.exe         the same for ARM
//   macOS    UwURDP-macos-universal.dmg             what people open
//            UwURDP-update-macos-universal          the setup program inside it, for updates
//   Linux    UwURDP-linux-<arch>.deb                the app for apt/dpkg, updates itself
//            UwURDP-linux-<arch>.rpm                the app for dnf/zypper/rpm, updates itself
//            UwURDP-linux-<arch>-portable.tar.gz    unpack and run, no updates
//            UwURDP-update-linux-x64.AppImage       x64 only: the setup, for copies an
//                                                   earlier setup installed; not a download
//
// Nothing here signs anything: `pnpm release` signs what the updater runs,
// under the versioned names installed apps expect, on the machine that holds
// the key. The builds below never see it.

import { execFileSync, execSync } from 'node:child_process';
import {
  chmodSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  renameSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const run = (command, env = {}) =>
  execSync(command, { cwd: root, stdio: 'inherit', env: { ...process.env, ...env } });

// Keep the update-signing key out of the builds: they run hundreds of
// third-party build scripts (Cargo build.rs, npm) that would otherwise see it.
delete process.env.TAURI_SIGNING_PRIVATE_KEY;
delete process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD;

const fail = (message) => {
  console.error(`\n✗ ${message}`);
  process.exit(1);
};

const targetIndex = process.argv.indexOf('--target');
const target = targetIndex > 0 ? process.argv[targetIndex + 1] : undefined;
const targetArg = target ? ` --target ${target}` : '';

const { version } = JSON.parse(
  readFileSync(join(root, 'apps/desktop/src-tauri/tauri.conf.json'), 'utf8'),
);
{
  const setup = JSON.parse(
    readFileSync(join(root, 'apps/setup/src-tauri/tauri.conf.json'), 'utf8'),
  );
  if (setup.version !== version) fail(`apps/setup says ${setup.version}, the app says ${version}.`);
}

const release = join(root, 'target', ...(target ? [target] : []), 'release');
const bundles = join(release, 'bundle');
const out = join(root, 'target', 'installers');
mkdirSync(out, { recursive: true });

/** The architecture as release file names say it. */
function arch() {
  if (target === 'universal-apple-darwin') return 'universal';
  const name = target ?? `${process.arch === 'arm64' ? 'aarch64' : 'x86_64'}-host`;
  return name.startsWith('aarch64') ? 'arm64' : 'x64';
}

/** A Tauri config override, as a file: quoting JSON on a command line is a trap on every shell. */
function configFile(name, config) {
  const path = join(mkdtempSync(join(tmpdir(), 'uwurdp-build-')), `${name}.json`);
  writeFileSync(path, JSON.stringify(config));
  return path;
}

function only(dir, test, what) {
  const found = existsSync(dir) ? readdirSync(dir).filter(test) : [];
  if (found.length !== 1) fail(`Expected one ${what} in ${dir}, found ${found.length}.`);
  return join(dir, found[0]);
}

const produced = [];

if (process.platform === 'win32') {
  console.log(`\n▸ Building UwURDP ${version}`);
  run(`pnpm --filter @uwurdp/desktop tauri build --no-bundle${targetArg}`);
  const app = join(release, 'uwurdp-desktop.exe');
  if (!existsSync(app)) fail(`Missing ${app}`);

  console.log('\n▸ Packing it into the setup');
  run(`pnpm --filter @uwurdp/setup tauri build --no-bundle${targetArg}`, {
    UWURDP_SETUP_PAYLOAD: app,
  });
  const setup = join(out, `UwURDP-windows-${arch()}-setup.exe`);
  copyFileSync(join(release, 'uwurdp-setup.exe'), setup);
  produced.push(setup);
} else if (process.platform === 'darwin') {
  // Ad-hoc signed: no Apple developer ID, but a sealed bundle, which Apple
  // silicon insists on and which keeps the app intact through the setup.
  const macOS = { signingIdentity: '-', minimumSystemVersion: '11.0' };
  const apps = mkdtempSync(join(tmpdir(), 'uwurdp-apps-'));

  console.log(`\n▸ Building UwURDP ${version}`);
  run(
    `pnpm --filter @uwurdp/desktop tauri build --bundles app${targetArg} --config "${configFile('app', { bundle: { macOS } })}"`,
  );
  execFileSync('ditto', [join(bundles, 'macos', 'UwURDP.app'), join(apps, 'UwURDP.app')]);

  console.log('\n▸ Packing it into the setup');
  const setupConfig = configFile('setup', {
    bundle: { active: true, targets: ['dmg'], macOS },
  });
  run(
    `pnpm --filter @uwurdp/setup tauri build --bundles dmg${targetArg} --config "${setupConfig}"`,
    {
      UWURDP_SETUP_PAYLOAD: apps,
    },
  );
  const dmg = join(out, `UwURDP-macos-${arch()}.dmg`);
  copyFileSync(
    only(join(bundles, 'dmg'), (name) => name.endsWith('.dmg'), 'disk image'),
    dmg,
  );
  const update = join(out, `UwURDP-update-macos-${arch()}`);
  copyFileSync(join(release, 'uwurdp-setup'), update);
  produced.push(dmg, update);
  rmSync(apps, { recursive: true, force: true });
} else {
  // The AppImage, unpacked, is both the portable folder and what the setup
  // installs: it runs from its AppDir, brings its own WebKit and needs no FUSE.
  const apps = mkdtempSync(join(tmpdir(), 'uwurdp-apps-'));
  const unpackAppImage = (image, name) => {
    const work = mkdtempSync(join(tmpdir(), 'uwurdp-appimage-'));
    execFileSync('chmod', ['+x', image]);
    execFileSync(image, ['--appimage-extract'], { cwd: work, stdio: 'ignore' });
    renameSync(join(work, 'squashfs-root'), join(apps, name));
    rmSync(work, { recursive: true, force: true });
  };

  console.log(`\n▸ Building UwURDP ${version}`);
  run(`pnpm --filter @uwurdp/desktop tauri build --bundles appimage${targetArg}`);
  unpackAppImage(
    only(join(bundles, 'appimage'), (name) => name.endsWith('.AppImage'), 'AppImage of UwURDP'),
    'UwURDP',
  );
  // The next build bundles into the same folders.
  rmSync(join(bundles, 'appimage'), { recursive: true, force: true });

  // The packages install system-wide, to /usr, as package `uwurdp`. Tauri
  // names the package after productName in kebab case, which would make
  // "UwURDP" `uw-urdp`; the menu entry keeps saying UwURDP through the
  // template in tauri.conf.json. The app looks for `uwurdp` when it checks that
  // dpkg or rpm owns it (apps/desktop/src-tauri/src/updates.rs).
  console.log(`\n▸ Packaging UwURDP ${version} as .deb and .rpm`);
  run(
    `pnpm --filter @uwurdp/desktop tauri build --bundles deb,rpm${targetArg} --config "${configFile('packages', { productName: 'uwurdp' })}"`,
  );
  const deb = join(out, `UwURDP-linux-${arch()}.deb`);
  copyFileSync(
    only(join(bundles, 'deb'), (name) => name.endsWith('.deb'), 'deb'),
    deb,
  );
  const rpm = join(out, `UwURDP-linux-${arch()}.rpm`);
  copyFileSync(
    only(join(bundles, 'rpm'), (name) => name.endsWith('.rpm'), 'rpm'),
    rpm,
  );
  produced.push(deb, rpm);

  // One folder, UwURDP/, that runs where it lands. tar, not zip: the AppDir
  // needs its modes and symlinks.
  console.log('\n▸ Packing the portable folder');
  const staging = mkdtempSync(join(tmpdir(), 'uwurdp-portable-'));
  const folder = join(staging, 'UwURDP');
  execFileSync('cp', ['-a', join(apps, 'UwURDP'), folder]);
  const launcher = join(folder, 'uwurdp');
  writeFileSync(
    launcher,
    '#!/bin/sh\n# Starts UwURDP from this folder.\nhere=$(dirname "$(readlink -f "$0")")\nexec "$here/AppRun" "$@"\n',
  );
  chmodSync(launcher, 0o755);
  writeFileSync(
    join(folder, 'README.txt'),
    [
      `UwURDP ${version}, portable`,
      '',
      'Start it with ./uwurdp (or ./AppRun) in this folder. Nothing is installed:',
      'the folder can live anywhere and be deleted when you are done.',
      '',
      'This copy does not update itself. For updates, install the .deb or .rpm',
      '(or the AUR package uwurdp-bin) instead, or fetch the newest',
      'UwURDP-linux-<arch>-portable.tar.gz from',
      'https://github.com/MinifyX/UwURDP-Client/releases/latest',
      '',
    ].join('\n'),
  );
  const portable = join(out, `UwURDP-linux-${arch()}-portable.tar.gz`);
  execFileSync('tar', [
    '--owner=0',
    '--group=0',
    '--numeric-owner',
    '-czf',
    portable,
    '-C',
    staging,
    'UwURDP',
  ]);
  rmSync(staging, { recursive: true, force: true });
  produced.push(portable);

  // Copies the setup installed (~/.local/share/uwurdp) update by running the
  // next setup. UwUSSH only ever shipped a Linux setup for x64, and UwURDP
  // keeps the same shape.
  if (arch() === 'x64') {
    console.log('\n▸ Packing it into the setup, for the updater');
    const setupConfig = configFile('setup', { bundle: { active: true, targets: ['appimage'] } });
    run(
      `pnpm --filter @uwurdp/setup tauri build --bundles appimage${targetArg} --config "${setupConfig}"`,
      { UWURDP_SETUP_PAYLOAD: apps },
    );
    const image = join(out, `UwURDP-update-linux-${arch()}.AppImage`);
    copyFileSync(
      only(
        join(bundles, 'appimage'),
        (name) => name.endsWith('.AppImage'),
        'AppImage of the setup',
      ),
      image,
    );
    execFileSync('chmod', ['+x', image]);
    produced.push(image);
  }
  rmSync(apps, { recursive: true, force: true });
}

console.log('\n▸ Nothing is signed for the updater here; `pnpm release` does that.');
for (const file of produced) console.log(`✧ ${file}`);
