// The whole end-to-end run, in one command:
//
//   node apps/desktop/e2e/run.mjs
//
// Starts the toy RDP server (`dev_rdpd`, a fresh certificate every start),
// starts the app in dev mode against a throwaway database with WebView2's
// DevTools port open on 127.0.0.1, and runs phase A: an RDCMan file is
// imported, a host connects through the certificate and login questions, the
// desktop draws and answers the mouse and keyboard, the overview shows it, and
// everything goes into an export. Then it connects two app instances to a
// real UwUSSH server for phase E (sync), and stops everything again.
//
//   node apps/desktop/e2e/run.mjs --only=a   just phase A
//   node apps/desktop/e2e/run.mjs --only=e   just phase E
//
// Phase E needs the server built next to this repository:
// ../UwUSSH-Server/target/debug/uwussh-server.exe (or UWURDP_SERVER_EXE).
//
// Windows only: it drives WebView2 over the Chrome DevTools Protocol.

import { execSync, spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const desktop = join(here, '..');
const repo = join(desktop, '..', '..');
const runDir = join(here, '.run');
const rdpdExe = join(repo, 'target', 'debug', 'examples', 'dev_rdpd.exe');
const workDir = join(runDir, 'work');
const only = process.argv.find((arg) => arg.startsWith('--only='))?.slice('--only='.length);
const serverExe =
  process.env.UWURDP_SERVER_EXE ??
  join(repo, '..', 'UwUSSH-Server', 'target', 'debug', 'uwussh-server.exe');
const appExe = join(repo, 'target', 'debug', 'uwurdp-desktop.exe');

rmSync(runDir, { recursive: true, force: true });
mkdirSync(runDir, { recursive: true });
mkdirSync(workDir, { recursive: true });
mkdirSync(join(here, 'shots'), { recursive: true });

const children = [];

function start(name, command, args, options = {}) {
  const log = join(runDir, `${name}.log`);
  // Straight into the file, not piped through this process. A pipe is only
  // drained while this event loop runs, so the phases would see a stale log —
  // and a busy process would eventually block on a full pipe.
  const fd = openSync(log, 'w');
  const child = spawn(command, args, {
    ...options,
    shell: command === 'pnpm',
    stdio: ['ignore', fd, fd],
  });
  children.push(child);
  return { child, log };
}

function stop(child) {
  // /T takes the whole tree: pnpm → vite and cargo → the app.
  spawnSync('taskkill', ['/PID', String(child.pid), '/T', '/F']);
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function until(condition, what, timeout = 600_000) {
  const started = Date.now();
  while (!(await condition())) {
    if (Date.now() - started > timeout) throw new Error(`gave up waiting for ${what}`);
    await sleep(500);
  }
}

async function startRdpd(name) {
  const rdpd = start(name, rdpdExe, [], {
    cwd: repo,
    env: { ...process.env, UWURDP_DEV_RDPD_ADDR: '127.0.0.1:3390' },
  });
  await until(
    () => existsSync(rdpd.log) && readFileSync(rdpd.log, 'utf8').includes('fingerprint'),
    'dev_rdpd',
  );
  const fingerprint = readFileSync(rdpd.log, 'utf8').match(/SHA256:[A-Za-z0-9+/]+/)?.[0];
  if (!fingerprint) throw new Error('dev_rdpd printed no fingerprint');
  return { ...rdpd, fingerprint };
}

/** Wait until the app's DevTools endpoint answers with the app's page. */
async function waitForApp(port = 9223) {
  await until(async () => {
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      return list.some((target) => target.url.startsWith('http://localhost:1420'));
    } catch {
      return false;
    }
  }, 'the app');
}

function startApp(name, db, extraEnv = {}) {
  return start(name, 'pnpm', ['tauri', 'dev'], {
    cwd: desktop,
    env: {
      ...process.env,
      UWURDP_DB: db,
      ...extraEnv,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:
        '--remote-debugging-port=9223 --remote-debugging-address=127.0.0.1',
      // A WebView2 folder of its own: an installed UwURDP that is running
      // shares the default one, and its browser process would ignore the
      // debugging port above.
      WEBVIEW2_USER_DATA_FOLDER: join(runDir, `webview-${name}`),
    },
  });
}

/** Run one phase without blocking this event loop, and report whether it passed. */
function phase(script, args) {
  console.log(`\n── ${script} ──`);
  return new Promise((resolve) => {
    spawn(process.execPath, [join(here, script), ...args], { stdio: 'inherit' }).on(
      'exit',
      (code) => resolve(code === 0),
    );
  });
}

/**
 * An RDCMan 2.7 file the way RDCMan writes one: a group whose login its
 * servers inherit (no password — RDCMan would seal that with DPAPI), a server
 * that is the dev server, and one nobody answers on.
 */
function rdcmanFile(path) {
  writeFileSync(
    path,
    `<?xml version="1.0" encoding="utf-8"?>
<RDCMan programVersion="2.7" schemaVersion="3">
  <file>
    <credentialsProfiles />
    <properties>
      <expanded>True</expanded>
      <name>Homelab</name>
    </properties>
    <group>
      <properties>
        <expanded>True</expanded>
        <name>Dev</name>
      </properties>
      <logonCredentials inherit="None">
        <profileName scope="Local">Custom</profileName>
        <userName>uwu</userName>
        <password />
        <domain />
      </logonCredentials>
      <server>
        <properties>
          <displayName>dev-rdpd</displayName>
          <name>127.0.0.1</name>
          <comment>the toy server</comment>
        </properties>
        <connectionSettings inherit="None">
          <connectToConsole>False</connectToConsole>
          <startProgram />
          <workingDir />
          <port>3390</port>
          <loadBalanceInfo />
        </connectionSettings>
      </server>
      <server>
        <properties>
          <displayName>nobody-home</displayName>
          <name>127.0.0.1</name>
        </properties>
        <connectionSettings inherit="None">
          <port>3399</port>
        </connectionSettings>
      </server>
    </group>
  </file>
  <connected />
  <favorites />
  <recentlyUsed />
</RDCMan>
`,
  );
}

/**
 * Phase E: a UwUSSH server on this machine with its own certificate, and two
 * app instances — one through `pnpm tauri dev`, one straight from the debug
 * binary that build left behind, on its own DevTools port and its own
 * database, against the same dev server page.
 */
async function phaseE() {
  if (!existsSync(serverExe)) {
    console.log(`\nphase E skipped: no ${serverExe} (build UwUSSH-Server first)`);
    return false;
  }
  const data = join(runDir, 'server');
  mkdirSync(data, { recursive: true });
  const serverEnv = {
    ...process.env,
    UWUSSH_DATA: data,
    UWUSSH_LISTEN: '127.0.0.1:18443',
    UWUSSH_PUBLIC: 'https://127.0.0.1:18443',
    UWUSSH_UPDATE_CHECK: 'off',
  };
  const invite = execSync(`"${serverExe}" invite`, { env: serverEnv, encoding: 'utf8' });
  const setupCode = invite.match(/uwu1_[A-Za-z0-9_-]+/)?.[0];
  if (!setupCode) throw new Error(`the server printed no setup code:\n${invite}`);
  const server = start('uwussh-server', serverExe, [], { env: serverEnv });
  await until(async () => {
    try {
      // Its own certificate: nothing here trusts it, which is the point.
      return readFileSync(server.log, 'utf8').includes('server ready');
    } catch {
      return false;
    }
  }, 'the UwUSSH server');

  const rdg = join(runDir, 'homelab-e.rdg');
  rdcmanFile(rdg);
  const first = startApp('app-e1', join(runDir, 'e1.db'), { UWURDP_E2E_OPEN_FILE: rdg });
  await waitForApp();
  const second = start('app-e2', appExe, [], {
    cwd: desktop,
    env: {
      ...process.env,
      UWURDP_DB: join(runDir, 'e2.db'),
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS:
        '--remote-debugging-port=9224 --remote-debugging-address=127.0.0.1',
      WEBVIEW2_USER_DATA_FOLDER: join(runDir, 'webview-app-e2'),
    },
  });
  await waitForApp(9224);
  const passed = await phase('phase-e.mjs', [setupCode, '9223', '9224']);
  stop(second.child);
  stop(first.child);
  stop(server.child);
  return passed;
}

try {
  if (only === 'e') {
    const ok = await phaseE();
    console.log(ok ? '\nEND TO END OK' : '\nEND TO END FAILED');
    for (const child of children) stop(child);
    spawnSync('taskkill', ['/IM', 'uwurdp-desktop.exe', '/T', '/F']);
    process.exit(ok ? 0 : 1);
  }
  // Always: cargo only rebuilds what changed, and a stale server tests nothing.
  execSync('cargo build -p uwurdp-core --example dev_rdpd', { cwd: repo, stdio: 'inherit' });

  const rdpd = await startRdpd('rdpd-a');
  const rdg = join(runDir, 'homelab.rdg');
  rdcmanFile(rdg);
  // Debug builds read UWURDP_E2E_* instead of showing native file dialogs.
  const app = startApp('app', join(runDir, 'e2e.db'), {
    UWURDP_E2E_SAVE_DIR: workDir,
    UWURDP_E2E_OPEN_FILE: rdg,
  });

  console.log('waiting for the app (the first build takes a while)…');
  await waitForApp();

  const passedA = await phase('phase-a.mjs', [rdpd.fingerprint, workDir]);
  stop(app.child);
  stop(rdpd.child);
  await sleep(1_000);

  const passedE = only === 'a' || (passedA && (await phaseE()));
  const ok = passedA && passedE;
  console.log(ok ? '\nEND TO END OK' : '\nEND TO END FAILED');
  for (const child of children) stop(child);
  spawnSync('taskkill', ['/IM', 'uwurdp-desktop.exe', '/T', '/F']);
  process.exit(ok ? 0 : 1);
} catch (error) {
  console.error(error);
  for (const child of children) stop(child);
  spawnSync('taskkill', ['/IM', 'uwurdp-desktop.exe', '/T', '/F']);
  process.exit(1);
}
