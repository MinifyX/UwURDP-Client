// Builds UwURDP for the Mac App Store: a universal app bundle (Apple Silicon
// and Intel) in an installer package, the form App Store Connect takes.
//
//   pnpm build:mas                     build, and sign when the identities are set
//   node scripts/build-mas.mjs --sign <UwURDP.app>
//                                      sign an app this script built elsewhere
//
// What comes out, in target/release:
//
//   UwURDP-<version>-mas-universal.pkg
//
// and the app itself in target/universal-apple-darwin/release/bundle/macos.
//
// The build is the desktop app alone — no setup app, which only the GitHub
// downloads need — without its updater and without the OpenH264 download
// (`--no-default-features --features mas`, see src-tauri/Cargo.toml), with
// tauri.mas.conf.json merged over tauri.conf.json: sandbox entitlements, the
// privacy manifest, no update feed. Tauri itself signs nothing here (`--no-sign`); this script does, so the
// order is ours: provisioning profile in, entitlements completed with the team,
// the app signed, the package signed.
//
// Signing, all from the environment, all optional — without them the result is
// unsigned, which is enough to prove the build works and nothing more:
//
//   APPLE_MAS_APP_IDENTITY        "3rd Party Mac Developer Application: … (TEAMID)"
//                                 or "Apple Distribution: … (TEAMID)"
//   APPLE_MAS_INSTALLER_IDENTITY  "3rd Party Mac Developer Installer: … (TEAMID)"
//   APPLE_TEAM_ID                 the ten-character team ID
//   APPLE_MAS_PROVISIONING_PROFILE  path to the Mac App Store provisioning
//                                 profile for app.uwurdp.desktop
//   MAS_BUILD_NUMBER              CFBundleVersion; must grow with every upload
//                                 of the same version. CI uses the run number.
//
// The identities have to be in a keychain codesign can reach. docs/app-store.md
// has where each of them comes from.

import { execFileSync } from 'node:child_process';
import {
  copyFileSync,
  existsSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from 'node:fs';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parseArgs } from 'node:util';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const tauriDir = join(root, 'apps/desktop/src-tauri');
const TARGET = 'universal-apple-darwin';
const BUNDLE_ID = 'app.uwurdp.desktop';

function fail(message) {
  console.error(`\n✗ ${message}`);
  process.exit(1);
}

function run(command, args, options = {}) {
  execFileSync(command, args, { cwd: root, stdio: 'inherit', ...options });
}

let options;
try {
  ({ values: options } = parseArgs({ options: { sign: { type: 'string' } } }));
} catch (error) {
  fail(`${error.message}\n  Usage: node scripts/build-mas.mjs [--sign <UwURDP.app>]`);
}
if (process.platform !== 'darwin')
  fail('The Mac App Store build needs a Mac: Xcode does the signing and packaging.');

const conf = JSON.parse(readFileSync(join(tauriDir, 'tauri.conf.json'), 'utf8'));
const env = process.env;
const signing = Boolean(env.APPLE_MAS_APP_IDENTITY);
if (options.sign && !signing)
  fail(
    '--sign needs APPLE_MAS_APP_IDENTITY (and the other signing variables): there is nothing to sign with.',
  );

/**
 * App Store Connect wants three plain numbers as the version, so a pre-release
 * suffix goes: 0.1.0-beta.12 is uploaded as 0.1.0, and the build number tells
 * the uploads of one version apart.
 */
const marketingVersion = conf.version.replace(/[-+].*$/, '');
const buildNumber = env.MAS_BUILD_NUMBER ?? '1';
if (!/^\d+(\.\d+){0,2}$/.test(buildNumber))
  fail(`MAS_BUILD_NUMBER "${buildNumber}" is not one to three numbers.`);
if (signing && !env.MAS_BUILD_NUMBER)
  fail('A signed build needs MAS_BUILD_NUMBER: App Store Connect refuses a build number twice.');
if (signing) {
  for (const name of [
    'APPLE_MAS_INSTALLER_IDENTITY',
    'APPLE_TEAM_ID',
    'APPLE_MAS_PROVISIONING_PROFILE',
  ]) {
    if (!env[name]) fail(`APPLE_MAS_APP_IDENTITY is set, so ${name} has to be too.`);
  }
  if (!existsSync(env.APPLE_MAS_PROVISIONING_PROFILE)) {
    fail(`No provisioning profile at ${env.APPLE_MAS_PROVISIONING_PROFILE}.`);
  }
}

const scratch = mkdtempSync(join(tmpdir(), 'uwurdp-mas-'));
try {
  const app = options.sign ? resolve(options.sign) : build();
  checkBundle(app);
  const pkg = join(root, 'target', 'release', `UwURDP-${conf.version}-mas-universal.pkg`);
  if (signing) sign(app);
  else console.log('\n▸ No APPLE_MAS_APP_IDENTITY: the app and the package stay unsigned.');
  rmSync(pkg, { force: true });
  console.log('\n▸ Packaging');
  run('productbuild', [
    '--component',
    app,
    '/Applications',
    ...(signing ? ['--sign', env.APPLE_MAS_INSTALLER_IDENTITY] : []),
    pkg,
  ]);
  if (!existsSync(pkg)) fail(`productbuild wrote no ${pkg}.`);
  console.log(`\n✧ ${pkg}${signing ? '' : ' (unsigned)'}`);
} finally {
  rmSync(scratch, { recursive: true, force: true });
}

function build() {
  const bundle = join(
    root,
    'target',
    TARGET,
    'release',
    'bundle',
    'macos',
    `${conf.productName}.app`,
  );
  rmSync(bundle, { recursive: true, force: true });

  // The parts that differ per build. A file, because quoting JSON on a command
  // line is different in every shell.
  const overrides = join(scratch, 'tauri.mas.build.json');
  writeFileSync(
    overrides,
    JSON.stringify({
      version: marketingVersion,
      bundle: {
        macOS: {
          bundleVersion: buildNumber,
          ...(signing
            ? {
                files: { 'embedded.provisionprofile': resolve(env.APPLE_MAS_PROVISIONING_PROFILE) },
              }
            : {}),
        },
      },
    }),
  );

  console.log(`\n▸ Building UwURDP ${marketingVersion} (${buildNumber}) for the Mac App Store`);
  run(
    'pnpm',
    [
      '--filter',
      '@uwurdp/desktop',
      'tauri',
      'build',
      '--target',
      TARGET,
      '--bundles',
      'app',
      '--no-sign',
      '--config',
      join(tauriDir, 'tauri.mas.conf.json'),
      '--config',
      overrides,
      '--features',
      'mas',
      '--',
      '--no-default-features',
    ],
    // The update-signing key has no business near this build: it has no
    // updater, and the build runs every build script in the dependency tree.
    { env: { ...env, TAURI_SIGNING_PRIVATE_KEY: '', TAURI_SIGNING_PRIVATE_KEY_PASSWORD: '' } },
  );
  if (!existsSync(bundle)) fail(`The build left no ${bundle}.`);
  return bundle;
}

/** What App Review would otherwise be the first to notice. */
function checkBundle(app) {
  const contents = join(app, 'Contents');
  const [exe] = readdirSync(join(contents, 'MacOS'));
  const archs = execFileSync('lipo', ['-archs', join(contents, 'MacOS', exe)], { encoding: 'utf8' })
    .trim()
    .split(/\s+/);
  if (!archs.includes('arm64') || !archs.includes('x86_64'))
    fail(`${exe} carries ${archs.join(' ')}, not arm64 and x86_64.`);

  const plist = (key) => {
    try {
      return execFileSync(
        '/usr/libexec/PlistBuddy',
        ['-c', `Print :${key}`, join(contents, 'Info.plist')],
        {
          encoding: 'utf8',
        },
      ).trim();
    } catch {
      return null;
    }
  };
  const expect = {
    CFBundleIdentifier: BUNDLE_ID,
    CFBundleShortVersionString: marketingVersion,
    LSApplicationCategoryType: 'public.app-category.utilities',
    ITSAppUsesNonExemptEncryption: 'true',
  };
  for (const [key, value] of Object.entries(expect)) {
    if (plist(key) !== value) fail(`Info.plist: ${key} is ${plist(key)}, expected ${value}.`);
  }
  if (!plist('LSMinimumSystemVersion')) fail('Info.plist has no LSMinimumSystemVersion.');
  if (!existsSync(join(contents, 'Resources', 'PrivacyInfo.xcprivacy')))
    fail('The privacy manifest is missing.');

  // uwurdp:// links have to reach the app (the deep-link plugin's scheme).
  if (plist('CFBundleURLTypes:0:CFBundleURLSchemes:0') !== 'uwurdp')
    fail('Info.plist registers no uwurdp:// scheme.');

  // The updater and the OpenH264 download must be gone, not merely unused:
  // a feed address in the binary would be a self-update mechanism as far as
  // App Review can tell, and Cisco's server a way to fetch code (2.5.2).
  const binary = readFileSync(join(contents, 'MacOS', exe));
  for (const [needle, what] of [
    ['raw.githubusercontent.com/MinifyX/UwURDP-Client/updates', 'The update feed'],
    ['ciscobinary.openh264.org', 'The OpenH264 download'],
  ]) {
    if (binary.includes(needle))
      fail(`${what} is still in the executable: was it built with --no-default-features?`);
  }
  console.log(
    `  ${exe}: ${archs.join(' ')}, version ${marketingVersion} (${plist('CFBundleVersion')})`,
  );
}

function sign(app) {
  console.log('\n▸ Signing');
  const profile = join(app, 'Contents', 'embedded.provisionprofile');
  // `--sign` on an app built elsewhere: the profile may not be in it yet.
  copyFileSync(env.APPLE_MAS_PROVISIONING_PROFILE, profile);

  // The two entitlements that name the team exist only at signing time, so the
  // file in the repository stays the same for everybody.
  const entitlements = join(scratch, 'Entitlements.mas.plist');
  copyFileSync(join(tauriDir, 'macos', 'Entitlements.mas.plist'), entitlements);
  for (const [key, value] of [
    ['com.apple.application-identifier', `${env.APPLE_TEAM_ID}.${BUNDLE_ID}`],
    ['com.apple.developer.team-identifier', env.APPLE_TEAM_ID],
  ]) {
    run('/usr/libexec/PlistBuddy', ['-c', `Add :${key} string ${value}`, entitlements]);
  }

  // One executable and no frameworks, so no inside-out signing of nested code:
  // the bundle's own signature covers everything in it.
  run('codesign', [
    '--force',
    '--timestamp',
    '--options',
    'runtime',
    '--entitlements',
    entitlements,
    '--sign',
    env.APPLE_MAS_APP_IDENTITY,
    app,
  ]);
  run('codesign', ['--verify', '--strict', '--verbose=2', app]);
  console.log(`  signed ${basename(app)} for team ${env.APPLE_TEAM_ID}`);
}
