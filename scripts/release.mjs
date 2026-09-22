// Publishes the UwURDP version in tauri.conf.json, for Windows, macOS and
// Linux, from this PC.
//
//   pnpm release                build the Windows x64 setup, fetch what CI built
//                               for the tag (Windows ARM, macOS, Linux), sign
//                               what the updater runs, check everything, publish it
//   pnpm release --no-build     use the Windows setup already in target/installers
//   pnpm release --windows-only only Windows x64, when CI can't help
//
// Needs a clean tree whose HEAD carries the pushed tag v<version>,
// release-notes/<version>.json, the GitHub CLI signed in with write access and
// the update signing key: TAURI_SIGNING_PRIVATE_KEY + TAURI_SIGNING_PRIVATE_KEY_PASSWORD,
// or a folder with uwurdp-update.key and PASSWORT.txt in UWURDP_UPDATE_KEY_DIR
// (default: Documents\UwURDP-Update-Schluessel).
//
// The key never leaves this machine: CI (.github/workflows/installers.yml)
// builds everything else unsigned when the tag is pushed, and this script
// downloads it and signs the files the updater runs here.
//
// The release's files carry no version in their names (UwURDP-windows-x64-setup.exe,
// UwURDP-linux-arm64.deb, …), so a link to the newest one never changes.
// Installed apps, though, accept an update only when its signature names the
// versioned file they expect (`UwURDP-Setup-<version>.exe`, …): that binds the
// signed bytes to the version the feed claims. So each file is signed as a
// copy under that versioned name, the same bytes are published under the
// plain name, and the feed points there — old and new apps both accept it.
//
// Creates the GitHub release with every file and a SHA256SUMS.txt, updates the
// feeds on the `updates` branch (creating it the first time) and writes the AUR
// package for it (see the end of this file).

import { execFileSync } from 'node:child_process';
import { createHash, createPublicKey, verify } from 'node:crypto';
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { homedir, tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { aurFiles, writeAur } from './aur.mjs';
import { FEED_BRANCH, REPOSITORY, releaseFeeds } from './release-feeds.mjs';

const root = join(dirname(fileURLToPath(import.meta.url)), '..');
const git = (args, cwd = root) => execFileSync('git', args, { cwd, encoding: 'utf8' }).trim();
const fail = (message) => {
  console.error(`\n✗ ${message}`);
  process.exit(1);
};
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

let token; // GitHub token for the API, looked up once
const build = !process.argv.includes('--no-build');
const windowsOnly = process.argv.includes('--windows-only');
const conf = JSON.parse(readFileSync(join(root, 'apps/desktop/src-tauri/tauri.conf.json'), 'utf8'));
const version = conf.version;
const tag = `v${version}`;
const installers = join(root, 'target', 'installers');
const windowsName = 'UwURDP-windows-x64-setup.exe';
const windowsSetup = join(installers, windowsName);

/**
 * What the release carries: each file under its published name, the CI
 * artifact it comes from (none: built here), and for the ones an installed app
 * updates itself with, `sign`: the feed's platform key → the versioned name the
 * signature has to carry. Those names are what installed apps check
 * (`setup_name` in apps/desktop/src-tauri/src/updates.rs); never change one,
 * or every install that expects it stops updating.
 */
const linux = (arch, rust) => [
  {
    artifact: `installers-linux-${arch}`,
    file: `UwURDP-linux-${arch}.deb`,
    sign: { [`linux-${rust}-deb`]: `UwURDP-${version}-linux-${rust}.deb` },
  },
  {
    artifact: `installers-linux-${arch}`,
    file: `UwURDP-linux-${arch}.rpm`,
    sign: { [`linux-${rust}-rpm`]: `UwURDP-${version}-linux-${rust}.rpm` },
  },
  { artifact: `installers-linux-${arch}`, file: `UwURDP-linux-${arch}-portable.tar.gz` },
];
const PLATFORMS = [
  {
    artifact: null,
    file: windowsName,
    sign: { 'windows-x86_64': `UwURDP-Setup-${version}.exe` },
  },
  {
    artifact: 'installers-windows-arm64',
    file: 'UwURDP-windows-arm64-setup.exe',
    sign: { 'windows-aarch64': `UwURDP-Setup-${version}-windows-arm64.exe` },
  },
  { artifact: 'installers-macos-universal', file: 'UwURDP-macos-universal.dmg' },
  {
    // One universal program for both Mac platforms, signed once under each name.
    artifact: 'installers-macos-universal',
    file: 'UwURDP-update-macos-universal',
    sign: {
      'darwin-aarch64': `UwURDP-Setup-${version}-macos-arm64-update`,
      'darwin-x86_64': `UwURDP-Setup-${version}-macos-x64-update`,
    },
  },
  ...linux('x64', 'x86_64'),
  ...linux('arm64', 'aarch64'),
  {
    // For copies an earlier setup AppImage installed into ~/.local/share/uwurdp.
    artifact: 'installers-linux-x64',
    file: 'UwURDP-update-linux-x64.AppImage',
    sign: { 'linux-x86_64': `UwURDP-Setup-${version}-linux-x64.AppImage` },
  },
].filter((entry) => !windowsOnly || entry.artifact === null);

console.log(`\n▸ Checking UwURDP ${version}`);
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.]+)?$/.test(version)) fail(`Unexpected version ${version}`);
if (git(['status', '--porcelain', '--untracked-files=no'])) {
  fail('The working tree has uncommitted changes.');
}
const head = git(['rev-parse', 'HEAD']);
let tagged = '';
try {
  tagged = git(['rev-parse', `${tag}^{commit}`]);
} catch {
  fail(`Tag ${tag} is missing. Tag the release commit and push the tag first.`);
}
if (tagged !== head) fail(`HEAD isn't ${tag}. Check out the tag first.`);
const pushed = git(['ls-remote', '--tags', 'origin', `refs/tags/${tag}`]).split(/\s/)[0];
if (pushed !== git(['rev-parse', `refs/tags/${tag}`])) fail(`Push ${tag} first.`);
if (!ghToken()) fail('Sign in to the GitHub CLI first (gh auth login).');
if (await github(`releases/tags/${tag}`)) fail(`${tag} is already released.`);
const notesFile = join(root, 'release-notes', `${version}.json`);
if (!existsSync(notesFile)) fail(`release-notes/${version}.json is missing.`);
const notes = JSON.parse(readFileSync(notesFile, 'utf8'));
if (typeof notes.de !== 'string' || typeof notes.en !== 'string') {
  fail('The release notes need de and en.');
}
const key = signingKey();

if (build) {
  console.log('\n▸ Building the Windows setup');
  // Without the key: the build runs third-party build scripts.
  const env = { ...process.env };
  delete env.TAURI_SIGNING_PRIVATE_KEY;
  delete env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD;
  execFileSync(process.execPath, [join(root, 'scripts', 'build-setup.mjs')], {
    cwd: root,
    stdio: 'inherit',
    env,
  });
} else if (!existsSync(windowsSetup)) {
  fail(`${windowsSetup} is missing. Run without --no-build.`);
} else if (statSync(windowsSetup).mtimeMs < Number(git(['log', '-1', '--format=%ct'])) * 1000) {
  fail('The setup in target/installers is older than the release commit. Run without --no-build.');
}

const work = mkdtempSync(join(tmpdir(), 'uwurdp-release-'));
try {
  const files = new Map([[windowsName, windowsSetup]]);
  if (!windowsOnly) {
    console.log('\n▸ Fetching what CI built for this tag: Windows ARM, macOS, Linux');
    const ci = join(work, 'ci');
    const run = await ciRun(head);
    execFileSync('gh', ['run', 'download', String(run), '--repo', REPOSITORY, '--dir', ci], {
      stdio: 'inherit',
    });
    for (const entry of PLATFORMS.filter((p) => p.artifact)) {
      const from = join(ci, entry.artifact, entry.file);
      if (!existsSync(from)) fail(`CI left no ${entry.file} in ${entry.artifact}.`);
      const to = join(installers, entry.file);
      mkdirSync(installers, { recursive: true });
      copyFileSync(from, to);
      files.set(entry.file, to);
    }
  }

  // Each under the versioned name its installed apps check for: a copy of the
  // same bytes, in a folder of its own, so `tauri signer` writes that name into
  // the signature's trusted comment.
  console.log('\n▸ Signing what the updater runs, and checking it against the updater key');
  const signed = {};
  const signing = join(work, 'sign');
  mkdirSync(signing);
  for (const entry of PLATFORMS.filter((p) => p.sign)) {
    const bytes = readFileSync(files.get(entry.file));
    for (const [platform, name] of Object.entries(entry.sign)) {
      const copy = join(signing, name);
      writeFileSync(copy, bytes);
      execFileSync(
        'pnpm',
        ['--filter', '@uwurdp/desktop', 'exec', 'tauri', 'signer', 'sign', copy],
        {
          cwd: root,
          stdio: 'inherit',
          shell: process.platform === 'win32',
          env: { ...process.env, ...key },
        },
      );
      if (!existsSync(`${copy}.sig`)) fail(`${name}.sig is missing: it wasn't signed.`);
      const signature = readFileSync(`${copy}.sig`, 'utf8').trim();
      checkSignature(bytes, signature, conf.plugins.updater.pubkey, name);
      signed[platform] = { name: entry.file, signature };
      console.log(`  ✓ ${platform}: ${entry.file}, signed as ${name}`);
    }
  }

  // A checksum for every file of the release, LF-only so `sha256sum -c` reads it.
  const sums = join(work, 'SHA256SUMS.txt');
  const lines = [...files.entries()].map(
    ([name, path]) => `${createHash('sha256').update(readFileSync(path)).digest('hex')}  ${name}`,
  );
  writeFileSync(sums, `${lines.join('\n')}\n`);
  files.set('SHA256SUMS.txt', sums);

  console.log(`\n▸ Creating the release on ${REPOSITORY}`);
  const notesPath = join(work, 'notes.md');
  writeFileSync(notesPath, releaseBody(await aurExists('uwurdp-bin')));
  const channel = version.includes('-') ? '--prerelease' : '--latest';
  execFileSync(
    'gh',
    [
      'release',
      'create',
      tag,
      ...files.values(),
      '--repo',
      REPOSITORY,
      '--verify-tag',
      '--title',
      `UwURDP ${version}`,
      '--notes-file',
      notesPath,
      channel,
    ],
    { stdio: 'inherit' },
  );

  const feeds = releaseFeeds({ version, notes, setups: signed });
  console.log(`\n▸ Updating ${Object.keys(feeds).join(', ')} on ${FEED_BRANCH}`);
  const dir = join(work, 'feeds');
  const remote = `https://github.com/${REPOSITORY}.git`;
  const exists = git(['ls-remote', '--heads', remote, FEED_BRANCH], work) !== '';
  if (exists) {
    git(['clone', '-q', '--depth', '1', '--branch', FEED_BRANCH, remote, dir], work);
  } else {
    // First release: the branch holds nothing but the feeds.
    git(['init', '-q', '-b', FEED_BRANCH, dir], work);
    git(['remote', 'add', 'origin', remote], dir);
    writeFileSync(
      join(dir, 'README.md'),
      '# Update feeds\n\nWritten by `pnpm release` on the main branch. Installed UwURDP apps read ' +
        '`stable.json` or `beta.json` from here, depending on their update channel.\n',
    );
  }
  for (const [name, feed] of Object.entries(feeds)) {
    writeFileSync(join(dir, name), `${JSON.stringify(feed, null, 2)}\n`);
  }
  git(['add', '--all'], dir);
  git(
    [
      '-c',
      'user.name=UwURDP Release',
      '-c',
      'user.email=release@uwurdp.invalid',
      'commit',
      '-qm',
      `UwURDP ${version}`,
    ],
    dir,
  );
  execFileSync('git', ['push', '-q', 'origin', `HEAD:${FEED_BRANCH}`], {
    cwd: dir,
    stdio: 'inherit',
  });

  console.log("\n▸ Checking what's online");
  const release = await github(`releases/tags/${tag}`);
  for (const [name, path] of files) {
    const asset = release?.assets?.find((a) => a.name === name);
    if (!asset || asset.state !== 'uploaded') fail(`The release has no ${name}.`);
    if (asset.size !== statSync(path).size) fail(`The published ${name} has a different size.`);
  }
  const feedName = version.includes('-') ? 'beta.json' : 'stable.json';
  const file = await github(`contents/${feedName}?ref=${FEED_BRANCH}`);
  const update = file && JSON.parse(Buffer.from(file.content, 'base64').toString('utf8'));
  if (update?.version !== version) fail(`${feedName} wasn't updated.`);
  for (const [platform, setup] of Object.entries(signed)) {
    const entry = update.platforms?.[platform];
    const asset = release.assets.find((a) => a.name === setup.name);
    if (entry?.signature !== setup.signature) fail(`${feedName} has no ${platform}.`);
    if (entry.url !== asset.browser_download_url)
      fail(`${feedName} points elsewhere for ${platform}.`);
  }
  console.log(`\n✧ UwURDP ${version} is out: ${release.html_url}`);
  console.log(
    `  ${feedName} updated for ${Object.keys(signed).join(', ')}${version.includes('-') ? ' (Beta channel)' : ''}`,
  );

  if (!windowsOnly) aur(readFileSync(sums, 'utf8'));
} finally {
  rmSync(work, { recursive: true, force: true });
}

/** The release page: what changed, then which file is for which system. */
function releaseBody(aurLive) {
  const guide = `https://github.com/${REPOSITORY}/blob/main/docs/install.md`;
  const has = (name) => PLATFORMS.some((p) => p.file === name);
  const code = (name) => `\`${name}\``;
  // [system (de), system (en), files] for each row whose files this release has.
  const rows = [
    ['Windows (x64)', 'Windows (x64)', code(windowsName)],
    ['Windows auf ARM', 'Windows on ARM', code('UwURDP-windows-arm64-setup.exe')],
    [
      'macOS (Intel & Apple-Chip)',
      'macOS (Intel & Apple chip)',
      code('UwURDP-macos-universal.dmg'),
    ],
    [
      'Ubuntu / Debian',
      'Ubuntu / Debian',
      `${code('UwURDP-linux-x64.deb')} · ARM: ${code('UwURDP-linux-arm64.deb')}`,
    ],
    [
      'Fedora / openSUSE',
      'Fedora / openSUSE',
      `${code('UwURDP-linux-x64.rpm')} · ARM: ${code('UwURDP-linux-arm64.rpm')}`,
    ],
    // The AUR package only once it exists there; AUR registration can be closed for a while.
    aurLive
      ? ['Arch Linux', 'Arch Linux', 'AUR: `yay -S uwurdp-bin`']
      : [
          'Arch Linux',
          'Arch Linux',
          `${code('UwURDP-linux-x64-portable.tar.gz')} (AUR: ${code('uwurdp-bin')} folgt / coming)`,
        ],
    [
      'Linux portabel',
      'Linux portable',
      `${code('UwURDP-linux-x64-portable.tar.gz')} · ARM: ${code('…-arm64-portable.tar.gz')}`,
    ],
  ].filter(([, , files]) => {
    const named = [...files.matchAll(/`(UwURDP-[^`]+)`/g)].map((m) => m[1]);
    return named.length === 0 ? !windowsOnly : named.every(has);
  });
  const table = (column) =>
    ['| | |', '|---|---|', ...rows.map((row) => `| ${row[column]} | ${row[2]} |`)].join('\n');
  const mac = has('UwURDP-macos-universal.dmg');
  const de = [
    table(0),
    'Windows: warnt es („Der Computer wurde durch Windows geschützt“), **Weitere Informationen → Trotzdem ausführen**.',
    ...(mac
      ? [
          'macOS: die `.dmg` öffnen und **UwURDP Setup** starten. UwURDP ist nicht bei Apple notarisiert (keine Developer ID): sagt macOS, es könne das Programm nicht prüfen, unter **Systemeinstellungen → Datenschutz & Sicherheit → Trotzdem öffnen** freigeben.',
        ]
      : []),
    ...(windowsOnly
      ? []
      : [
          'Linux: `.deb` und `.rpm` installieren systemweit und aktualisieren sich selbst (fragt nach dem Administrator-Passwort); die portable Version einfach entpacken und `./UwURDP/uwurdp` starten, sie aktualisiert sich nicht.',
        ]),
  ];
  const en = [
    table(1),
    'Windows: if it warns that it "protected your PC", **More info → Run anyway**.',
    ...(mac
      ? [
          "macOS: open the `.dmg` and start **UwURDP Setup**. UwURDP isn't notarized by Apple (no developer ID): if macOS says it can't check the app, allow it under **System Settings → Privacy & Security → Open Anyway**.",
        ]
      : []),
    ...(windowsOnly
      ? []
      : [
          "Linux: the `.deb` and `.rpm` install system-wide and update themselves (asking for the administrator password); the portable one you just unpack and start with `./UwURDP/uwurdp`, and it doesn't update itself.",
        ]),
  ];
  return [
    `## Deutsch\n\n${notes.de}\n`,
    `## English\n\n${notes.en}\n`,
    `## Herunterladen\n\n${de.join('\n\n')}\n\n[Anleitung](${guide})\n`,
    `## Downloads\n\n${en.join('\n\n')}\n\n[Install guide](${guide})\n`,
    'Prüfsummen · checksums: `SHA256SUMS.txt`. Die `UwURDP-update-…`-Dateien sind für den Updater in der App · the `UwURDP-update-…` files are for the in-app updater.\n',
  ].join('\n');
}

/**
 * The AUR package uwurdp-bin for this release. With UWURDP_AUR_DIR pointing at
 * a checkout of ssh://aur@aur.archlinux.org/uwurdp-bin.git it is committed and
 * pushed from there; otherwise it lands in target/aur/uwurdp-bin, and CI's
 * `aur` job (.github/workflows/aur.yml) pushes it once AUR_SSH_PRIVATE_KEY is set.
 */
function aur(sums) {
  const files = aurFiles({ version, sums });
  const checkout = process.env.UWURDP_AUR_DIR;
  if (!checkout) {
    const dir = join(root, 'target', 'aur', 'uwurdp-bin');
    writeAur(dir, files);
    console.log(`\n▸ AUR: PKGBUILD and .SRCINFO in ${dir}`);
    console.log(
      '  CI pushes them (aur.yml) when AUR_SSH_PRIVATE_KEY is set. By hand: copy both into a\n' +
        '  checkout of ssh://aur@aur.archlinux.org/uwurdp-bin.git, commit and push — or set\n' +
        '  UWURDP_AUR_DIR to that checkout next time.',
    );
    return;
  }
  if (!existsSync(join(checkout, '.git'))) fail(`UWURDP_AUR_DIR (${checkout}) is no git checkout.`);
  console.log(`\n▸ AUR: uwurdp-bin ${version} from ${checkout}`);
  git(['pull', '-q', '--ff-only'], checkout);
  writeAur(checkout, files);
  git(['add', 'PKGBUILD', '.SRCINFO'], checkout);
  if (!git(['status', '--porcelain'], checkout)) {
    console.log('  already up to date');
    return;
  }
  git(['commit', '-qm', `UwURDP ${version}`], checkout);
  execFileSync('git', ['push', '-q'], { cwd: checkout, stdio: 'inherit' });
  console.log('  ✓ pushed');
}

/** The Installers run for this commit, once it has finished green. */
async function ciRun(sha) {
  const started = Date.now();
  let announced = false;
  for (;;) {
    const runs = JSON.parse(
      execFileSync(
        'gh',
        [
          'run',
          'list',
          '--repo',
          REPOSITORY,
          '--workflow',
          'installers.yml',
          '--commit',
          sha,
          '--json',
          'databaseId,status,conclusion,event',
          '--limit',
          '10',
        ],
        { encoding: 'utf8' },
      ),
    );
    const done = runs.find((run) => run.status === 'completed' && run.conclusion === 'success');
    if (done) return done.databaseId;
    const running = runs.find((run) => run.status !== 'completed');
    if (!running && runs.length > 0 && Date.now() - started > 60_000) {
      fail(
        `The Installers run for ${sha.slice(0, 7)} failed. Fix it, or release with --windows-only.`,
      );
    }
    if (Date.now() - started > 90 * 60_000) fail('Gave up waiting for CI after 90 minutes.');
    if (!announced) {
      console.log('  waiting for CI to finish the macOS and Linux builds…');
      announced = true;
    }
    await sleep(30_000);
  }
}

/** The update signing key from the environment or the key folder. */
function signingKey() {
  if (process.env.TAURI_SIGNING_PRIVATE_KEY) {
    return {
      TAURI_SIGNING_PRIVATE_KEY: process.env.TAURI_SIGNING_PRIVATE_KEY,
      TAURI_SIGNING_PRIVATE_KEY_PASSWORD: process.env.TAURI_SIGNING_PRIVATE_KEY_PASSWORD ?? '',
    };
  }
  const folder =
    process.env.UWURDP_UPDATE_KEY_DIR || join(homedir(), 'Documents', 'UwURDP-Update-Schluessel');
  const file = join(folder, 'uwurdp-update.key');
  if (!existsSync(file)) {
    fail('No signing key: set TAURI_SIGNING_PRIVATE_KEY(_PASSWORD) or UWURDP_UPDATE_KEY_DIR.');
  }
  const password = join(folder, 'PASSWORT.txt');
  return {
    TAURI_SIGNING_PRIVATE_KEY: readFileSync(file, 'utf8').trim(),
    TAURI_SIGNING_PRIVATE_KEY_PASSWORD: existsSync(password)
      ? readFileSync(password, 'utf8').trim()
      : '',
  };
}

/** Verifies a Tauri updater signature (base64 minisign) the way installed apps do. */
function checkSignature(file, signatureBase64, pubkeyBase64, expectedName) {
  const lines = (text) => Buffer.from(text, 'base64').toString('utf8').split(/\r?\n/);
  const pub = Buffer.from(lines(pubkeyBase64)[1], 'base64');
  const [, signatureLine, trustedLine, globalLine] = lines(signatureBase64);
  const sig = Buffer.from(signatureLine, 'base64');
  if (pub.length !== 42 || sig.length !== 74) fail('Malformed key or signature.');
  if (!sig.subarray(2, 10).equals(pub.subarray(2, 10))) {
    fail(`${expectedName} was signed with a different key.`);
  }
  const publicKey = createPublicKey({
    key: { kty: 'OKP', crv: 'Ed25519', x: pub.subarray(10).toString('base64url') },
    format: 'jwk',
  });
  const algorithm = sig.subarray(0, 2).toString('latin1');
  const signed = algorithm === 'ED' ? createHash('blake2b512').update(file).digest() : file;
  if (!verify(null, signed, publicKey, sig.subarray(10))) {
    fail(`The signature doesn't match ${expectedName}.`);
  }
  const trusted = Buffer.from(trustedLine.replace(/^trusted comment: /, ''), 'utf8');
  if (
    !verify(
      null,
      Buffer.concat([sig.subarray(10), trusted]),
      publicKey,
      Buffer.from(globalLine, 'base64'),
    )
  ) {
    fail(`The trusted comment of ${expectedName}'s signature doesn't verify.`);
  }
  // Installed apps refuse a setup whose signature names another file: that is
  // what ties the feed's version to the signed setup.
  const names = trusted
    .toString('utf8')
    .split('\t')
    .map((part) => part.trim().replace(/^file:/, ''));
  if (!names.includes(expectedName)) fail(`The signature doesn't name ${expectedName}.`);
}

/** Whether the AUR has this package; false when it doesn't or doesn't answer. */
async function aurExists(name) {
  try {
    const response = await fetch(`https://aur.archlinux.org/rpc/v5/info?arg[]=${name}`, {
      headers: { 'user-agent': 'uwurdp-release' },
      signal: AbortSignal.timeout(15_000),
    });
    return response.ok && (await response.json()).resultcount > 0;
  } catch {
    return false;
  }
}

async function github(path) {
  const headers = { accept: 'application/vnd.github+json', 'user-agent': 'uwurdp-release' };
  token ??= process.env.GH_TOKEN || process.env.GITHUB_TOKEN || ghToken();
  if (token) headers.authorization = `Bearer ${token}`;
  const response = await fetch(`https://api.github.com/repos/${REPOSITORY}/${path}`, { headers });
  if (response.status === 404) return null;
  if (!response.ok) throw new Error(`GitHub answered ${response.status} for ${path}`);
  return response.json();
}

function ghToken() {
  try {
    return execFileSync('gh', ['auth', 'token'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim();
  } catch {
    return '';
  }
}
