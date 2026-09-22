// Phase E: two app instances and a real UwUSync server.
//
// The first device connects the server with a setup code, is shown the
// recovery kit once, imports a host, and offers a pairing code. The second
// device types the short code with the server's address and fingerprint, the
// account's master password, and gets the host. A group made on the second
// device reaches the first. Then the first revokes the second, with the master
// password, and the second's next pass is refused.
//
// Arguments: the setup code, the DevTools ports of both apps.
import { check, connect, failed, sleep } from './cdp.mjs';

const SHOTS = new URL('./shots/', import.meta.url).pathname.replace(/^\/([A-Z]:)/, '$1');
const [SETUP_CODE, PORT_A = '9223', PORT_B = '9224'] = process.argv.slice(2);
const MASTER = 'sync-master-pw';

const a = await connect(Number(PORT_A));
const b = await connect(Number(PORT_B));
const top = `[...document.querySelectorAll('.modal')].pop()`;
const invoke = (page, command, args = {}) =>
  page.eval(
    `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(args)})`,
  );

for (const page of [a, b]) {
  await page.waitFor(`document.querySelector('.sidebar')`, { what: 'app shell' });
}
await sleep(500);
check(
  'nothing opens on start by default',
  (await a.eval(`document.querySelectorAll('.tab').length`)) === 0 &&
    (await b.eval(`document.querySelectorAll('.tab').length`)) === 0,
);

async function openSync(page) {
  await page.click('[aria-label="Einstellungen"]');
  await page.waitFor(`document.querySelector('.settings-nav')`, { what: 'settings' });
  await page.click('.settings-nav button', 'Sync');
  await page.waitFor(`document.querySelector('.sync-intro, .setting-row')`, {
    what: 'sync section',
  });
}

// ── First device: connect the server ────────────────────────────────────────
await openSync(a);
check(
  'an unpaired device offers both ways in',
  (await a.text('.sync-choice')).includes('Server verbinden') &&
    (await a.text('.sync-choice')).includes('Mit einem Gerät koppeln'),
);
await a.screenshot(`${SHOTS}e0-sync-intro.png`);
await a.click('.sync-choice', 'Server verbinden');
await a.waitFor(`document.querySelector('.sync-code-input')`, { what: 'connect form' });
await a.click('.sync-code-input');
await a.type('uwu1_not-a-real-code');
check(
  'a code that is not one keeps the button off',
  await a.eval(`${top}.querySelector('.sync-form button[type=submit]').disabled`),
);
await a.fill('.sync-code-input', SETUP_CODE);
await a.eval(`${top}.querySelectorAll('.sync-form input[type=password]')[0].focus()`);
await a.type(MASTER);
await a.eval(`${top}.querySelectorAll('.sync-form input[type=password]')[1].focus()`);
await a.type(MASTER);
await a.click('.sync-form button[type=submit]');
await a.waitFor(`document.querySelector('.sync-kit-code')`, {
  what: 'recovery kit',
  timeout: 60_000,
});
const recovery = (await a.text('.sync-kit-code')).trim();
check(
  'the recovery kit shows the account key in its printed form',
  /^[0-9A-Z]{7}(-[0-9A-Z]{7}){3}$/.test(recovery),
  recovery,
);
check(
  'the kit can only be closed once it is saved',
  await a.eval(`[...document.querySelectorAll('.sync-actions button')].pop().disabled`),
);
await a.screenshot(`${SHOTS}e1-recovery-kit.png`);
await a.click('.sync-kit .check input');
await a.click('.sync-actions button', 'Fertig');
await a.waitFor(
  `(document.querySelector('.settings-content')?.textContent ?? '').includes('Verbunden mit')`,
  {
    what: 'paired status',
  },
);
check('the first device is paired', (await invoke(a, 'sync_status')).paired);

// The kit again, later: only for the master password.
const wrongKit = await a.eval(
  `window.__TAURI_INTERNALS__.invoke('sync_recovery_code', { password: 'nope' }).then(() => null, (e) => e.kind)`,
);
check('the kit is not shown again for a wrong password', wrongKit === 'password-wrong', wrongKit);
await a.click('.setting-row button', 'Anzeigen');
await a.waitFor(`${top}?.querySelector('input[type=password]')`, { what: 'kit password prompt' });
await a.eval(`${top}.querySelector('input[type=password]').focus()`);
await a.type(MASTER);
await a.click('.modal-footer button', 'Anzeigen');
await a.waitFor(`document.querySelector('.sync-kit-code')`, { what: 'kit again', timeout: 30_000 });
check(
  'a paired device shows the same recovery code again',
  (await a.text('.sync-kit-code')).trim() === recovery,
);
await a.click('.sync-actions button', 'Fertig');
await a.waitFor(
  `(document.querySelector('.settings-content')?.textContent ?? '').includes('Verbunden mit')`,
  { what: 'paired status after the kit' },
);

// Close settings, import hosts from an RDCMan file.
await a.click('.settings-close');
await a.click('.sidebar-head [aria-label="Importieren"]');
await a.waitFor(`document.querySelector('.import-sources')`, { what: 'source picker' });
await a.click('.import-source', 'RDCMan-Datei (.rdg)');
await a.waitFor(`document.querySelector('.import-preview')`, { what: 'rdg preview' });
await a.click('.modal-footer button', 'Importieren');
await a.waitFor(
  `[...document.querySelectorAll('.modal-title')].pop()?.textContent.startsWith('Import abgeschlossen')`,
  { what: 'import done' },
);
await a.click('.modal-footer button', 'Fertig');
await a.waitFor(
  `[...document.querySelectorAll('.host-name')].some(e => e.textContent === 'dev-rdpd')`,
  {
    what: 'imported host',
  },
);
await invoke(a, 'sync_now');
await a.waitFor(
  `window.__TAURI_INTERNALS__.invoke('sync_status').then(s => s.pending === 0 && s.last && !s.last.error)`,
  { what: 'the host pushed', timeout: 30_000 },
);
check('the imported host went up to the server', true);

// ── Add a device ────────────────────────────────────────────────────────────
await openSync(a);
await a.waitFor(`document.querySelector('.setting-row button')`, { what: 'paired section' });
await a.click('.setting-row button', 'Gerät hinzufügen');
await a.waitFor(`${top}?.querySelector('input[type=password]')`, { what: 'add-device password' });
await a.eval(`${top}.querySelector('input[type=password]').focus()`);
await a.type(MASTER);
await a.click('.modal-footer button', 'Code zeigen');
await a.waitFor(`document.querySelector('.sync-offer .sync-kit-code')`, {
  what: 'pairing code',
  timeout: 20_000,
});
const spoken = (await a.text('.sync-offer .sync-kit-code')).trim();
check(
  'the pairing code is an id and three words',
  /^[0-9A-Z]{5}(-[a-zäöü]+){3}$/u.test(spoken),
  spoken,
);
const fingerprint = (await a.text('.sync-offer .sync-fingerprint')).trim();
const status = await invoke(a, 'sync_status');
await a.screenshot(`${SHOTS}e2-add-device.png`);

// ── Second device: join with the short code ─────────────────────────────────
await openSync(b);
await b.click('.sync-choice', 'Mit einem Gerät koppeln');
await b.waitFor(`document.querySelector('.sync-code-input')`, { what: 'join form' });
await b.click('.sync-code-input');
await b.type(spoken);
await b.waitFor(`document.querySelector('.sync-form input[placeholder^="https"]')`, {
  what: 'server address field for a short code',
});
check('a short code asks for the server it belongs to', true);
await b.click('.sync-form input[placeholder^="https"]');
await b.type(status.serverUrl);
await b.click('.sync-form input[placeholder^="SHA256"]');
await b.type(fingerprint);
await b.click('.sync-form input[type=password]');
await b.type(MASTER);
await b.click('.sync-form button[type=submit]');

await a.waitFor(
  `(document.querySelector('.settings-content')?.textContent ?? '').includes('ist beigetreten')`,
  {
    what: 'the first device sees the second join',
    timeout: 90_000,
  },
);
check('the first device says who joined', true);
await b.waitFor(
  `(document.querySelector('.settings-content')?.textContent ?? '').includes('Verbunden mit')`,
  {
    what: 'second device paired',
    timeout: 30_000,
  },
);
check('the second device is paired', (await invoke(b, 'sync_status')).paired);
await b.screenshot(`${SHOTS}e3-joined.png`);

await b.click('.settings-close');
await b.waitFor(
  `[...document.querySelectorAll('.host-name')].some(e => e.textContent === 'dev-rdpd')`,
  {
    what: 'the host arrives on the second device',
    timeout: 30_000,
  },
);
check('the host from the first device arrived on the second', true);
check(
  'the second device opens the vault it joined without asking',
  (await invoke(b, 'vault_state')).status === 'unlocked',
);

// A change on the second device reaches the first.
await invoke(b, 'create_group', { workspace: 'private', name: 'vom-laptop' });
await invoke(b, 'sync_now');
await b.waitFor(
  `window.__TAURI_INTERNALS__.invoke('sync_status').then(s => s.pending === 0 && s.last && !s.last.error)`,
  { what: 'the group pushed', timeout: 30_000 },
);
await invoke(a, 'sync_now');
await a.waitFor(
  `window.__TAURI_INTERNALS__.invoke('list_groups').then(g => g.some(x => x.name === 'vom-laptop'))`,
  { what: 'the group reaches the first device', timeout: 30_000 },
);
check('a group made on the second device reached the first', true);

// ── Devices and revoking ────────────────────────────────────────────────────
await a.waitFor(`document.querySelectorAll('.sync-device-list li').length === 2`, {
  what: 'both devices listed',
  timeout: 20_000,
});
check(
  'the device list marks this device',
  (await a.text('.sync-device-list li')).includes('dieses Gerät'),
);
await a.click('.sync-device-list button', 'Widerrufen');
await a.waitFor(`${top}?.querySelector('input[type=password]')`, { what: 'revoke password' });
await a.click('.modal[data-tone="warning"] input[type=password]');
await a.type('wrong-password');
await a.key('Enter');
await a.waitFor(`${top}?.querySelector('.field-error')`, {
  what: 'wrong password refused',
  timeout: 20_000,
});
check('revoking with the wrong password is refused', true);
await a.click('.modal[data-tone="warning"] input[type=password]');
await a.type(MASTER);
await a.key('Enter');
await a.waitFor(`document.querySelector('.sync-devices .import-warning')`, {
  what: 'revoked',
  timeout: 20_000,
});
check(
  'revoking says honestly what the device still knows',
  (await a.text('.sync-devices .import-warning')).includes('Master-Passwort'),
);
await a.screenshot(`${SHOTS}e4-revoked.png`);

await invoke(b, 'sync_now');
await b.waitFor(
  `window.__TAURI_INTERNALS__.invoke('sync_status').then(s => s.last && s.last.error)`,
  { what: 'the revoked device is refused', timeout: 30_000 },
);
check('the revoked device can no longer sync', true);

// The revoked device leaves: its vault goes back to the master password alone.
await b.click('[aria-label="Einstellungen"]');
await b.waitFor(`document.querySelector('.settings-nav')`, {
  what: 'settings on the second device',
});
await b.click('.settings-nav button', 'Sync');
await b.waitFor(
  `[...document.querySelectorAll('.setting-row button')].some(e => e.textContent.includes('Trennen'))`,
  {
    what: 'disconnect button',
  },
);
await b.click('.setting-row button', 'Trennen');
await b.waitFor(`${top}?.querySelector('input[type=password]')`, { what: 'disconnect password' });
await b.click('.modal[data-tone="warning"] input[type=password]');
await b.type(MASTER);
await b.key('Enter');
await b.waitFor(`document.querySelector('.sync-intro')`, {
  what: 'unpaired again',
  timeout: 30_000,
});
check('a device can leave, and is unpaired afterwards', !(await invoke(b, 'sync_status')).paired);
await invoke(b, 'lock_vault');
await invoke(b, 'unlock_vault', { password: MASTER, remember: null, recoveryCode: null });
check(
  'after leaving, the master password alone opens the vault again',
  (await invoke(b, 'vault_state')).status === 'unlocked',
);
check(
  'the hosts stay on a device that left',
  (await invoke(b, 'list_hosts')).some((host) => host.name === 'dev-rdpd'),
);

a.close();
b.close();
process.exit(failed() > 0 ? 1 : 0);
