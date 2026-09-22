// Phase A: the everyday path, against dev_rdpd.
//
// An RDCMan file comes in (a group whose login its servers inherit), a host
// connects: the certificate is asked about once and matches the server's, the
// login is prefilled from the group and only the password is typed, and saved
// into a vault made on the way. The desktop draws, the mouse and the keyboard
// reach it, the overview shows it live, a disconnect keeps the tab, and a
// reconnect asks nothing. A host that nobody answers on fails plainly. At the
// end, everything goes into an export file.
//
// Arguments: the dev server's certificate fingerprint, the folder exports land in.
import { readdirSync } from 'node:fs';
import { check, connect, failed, sleep } from './cdp.mjs';

const SHOTS = new URL('./shots/', import.meta.url).pathname.replace(/^\/([A-Z]:)/, '$1');
const [FINGERPRINT, WORK_DIR] = process.argv.slice(2);
const MASTER = 'e2e-master-pw';

const page = await connect();
const top = `[...document.querySelectorAll('.modal')].pop()`;
const topTitle = `${top}?.querySelector('.modal-title')?.textContent ?? ''`;
const driver = `window.__uwurdpDriver`;

await page.waitFor(`document.querySelector('.sidebar')`, { what: 'app shell' });
check(
  'nothing opens on start',
  (await page.eval(`document.querySelectorAll('.tab').length`)) === 0,
);
check(
  'an empty host list offers the RDCMan import',
  (await page.text('.sidebar-empty')).includes('RDCMan'),
);
await page.screenshot(`${SHOTS}a0-empty.png`);

// ── Import an RDCMan file ───────────────────────────────────────────────────
await page.click('.sidebar-head [aria-label="Importieren"]');
await page.waitFor(`document.querySelector('.import-sources')`, { what: 'source picker' });
const sources = await page.text('.import-source');
check(
  'the picker offers RDCMan, .rdp files and exports',
  ['RDCMan-Datei', 'RDP-Dateien', 'UwURDP-Export'].every((word) => sources.includes(word)),
);
await page.click('.import-source', 'RDCMan-Datei (.rdg)');
await page.waitFor(`document.querySelector('.import-preview')`, { what: 'rdg preview' });
const preview = await page.text('.import-counts li');
check(
  'the preview counts two hosts, a group and a login',
  /2\s*Hosts/.test(preview) && /1\s*Gruppen/.test(preview) && /1\s*Anmeldungen/.test(preview),
  preview,
);
await page.screenshot(`${SHOTS}a1-import-preview.png`);
await page.click('.modal-footer button', 'Importieren');
await page.waitFor(`(${topTitle}).startsWith('Import abgeschlossen')`, { what: 'import done' });
await page.click('.modal-footer button', 'Fertig');
await page.waitFor(
  `[...document.querySelectorAll('.host-name')].some(e => e.textContent === 'dev-rdpd')`,
  { what: 'imported host' },
);
check(
  'the group arrived and says its hosts inherit a login',
  (await page.text('.group-head h3')).includes('Dev') &&
    (await page.eval(`!!document.querySelector('.group-head .group-login')`)),
);
check(
  'the comment from RDCMan came along',
  (await page.eval(
    `window.__TAURI_INTERNALS__.invoke('list_hosts').then(h => h.find(x => x.name === 'dev-rdpd').comment)`,
  )) === 'the toy server',
);

// ── Connect: login, certificate, vault ──────────────────────────────────────
// The group's user comes prefilled; RDCMan's file had no password for it.
await page.click('.host .host-name', 'dev-rdpd');
await page.waitFor(`(${topTitle}) === 'Anmeldung'`, { what: 'login prompt', timeout: 20_000 });
check(
  "the login prompt is prefilled with the group's user",
  (await page.eval(`${top}.querySelector('input').value`)) === 'uwu',
);
// A wrong password first, not saved.
await page.eval(`${top}.querySelector('input[type=password]').focus()`);
await page.type('wrong');
await page.eval(`${top}.querySelector('.check input').click()`);
await page.key('Enter');

// Nothing is sent before the certificate is settled.
await page.waitFor(`(${topTitle}) === 'Unbekanntes Zertifikat'`, {
  what: 'certificate dialog',
  timeout: 20_000,
});
const shown = await page.text(`.modal .fingerprint`);
check('the certificate dialog shows the server fingerprint', shown.includes(FINGERPRINT), shown);
check(
  'it shows a Windows-style thumbprint as well',
  /[0-9A-F]{40}/.test(await page.text('.modal .key-compare')),
);
check(
  'no button in the certificate dialog has the focus',
  await page.eval(`document.activeElement?.tagName !== 'BUTTON'`),
);
await page.screenshot(`${SHOTS}a2-certificate.png`);
await page.click('.modal-footer button', 'Vertrauen und verbinden');

await page.waitFor(`(${topTitle}) === 'Anmeldung' && ${top}.querySelector('.field-error')`, {
  what: 'rejected login',
  timeout: 20_000,
});
check('a wrong password is asked for again, with the reason', true);
await page.eval(`${top}.querySelector('input[type=password]').focus()`);
await page.type('nyu');
await page.eval(
  `(() => { const c = ${top}.querySelector('.check input'); if (!c.checked) c.click(); })()`,
);
await page.key('Enter');

await page.waitFor(`${driver}?.session`, { what: 'a live session', timeout: 30_000 });
check('the session is live', true);
// Saving the password makes a vault on the way.
await page.waitFor(`(${topTitle}) === 'Tresor anlegen'`, { what: 'vault dialog', timeout: 20_000 });
const passwords = await page.eval(`${top}.querySelectorAll('input[type=password]').length`);
await page.eval(`${top}.querySelectorAll('input[type=password]')[0].focus()`);
await page.type(MASTER);
if (passwords > 1) {
  await page.eval(`${top}.querySelectorAll('input[type=password]')[1].focus()`);
  await page.type(MASTER);
}
await page.click('.modal-footer button', 'Anlegen');
await page.waitFor(`!document.querySelector('.modal')`, { what: 'vault made', timeout: 30_000 });
await page.waitFor(
  `window.__TAURI_INTERNALS__.invoke('list_hosts').then(h => h.find(x => x.name === 'dev-rdpd').hasPassword)`,
  { what: 'the login kept', timeout: 20_000 },
);
check('the typed login is kept for the host, in the vault', true);

// ── The desktop ─────────────────────────────────────────────────────────────
await page.waitFor(`${driver}.canvas.width > 200 && ${driver}.canvas.height > 200`, {
  what: 'desktop size',
  timeout: 20_000,
});
const pixel = (x, y) =>
  page.eval(
    `Array.from(${driver}.canvas.getContext('2d').getImageData(${x}, ${y}, 1, 1).data).join(',')`,
  );
await page.waitFor(
  `${driver}.canvas.getContext('2d').getImageData(10, 10, 1, 1).data[3] === 255 && ${driver}.canvas.getContext('2d').getImageData(10, 10, 1, 1).data.slice(0,3).some(v => v > 40)`,
  {
    what: 'pixels drawn',
    timeout: 20_000,
  },
);
// A notice from connecting goes away once live; the desktop then takes the tab's size.
await page
  .waitFor(
    `(() => { const r = document.querySelector('.session-pane:not([hidden]) .rdp-view').getBoundingClientRect(); return ${driver}.canvas.width === Math.round(r.width * devicePixelRatio) && ${driver}.canvas.height === Math.round(r.height * devicePixelRatio); })()`,
    { what: 'the desktop to fit its tab', timeout: 8_000 },
  )
  .catch(() => undefined);
const size = await page.eval(`[${driver}.canvas.width, ${driver}.canvas.height].join('x')`);
const viewport = await page.eval(
  `(() => { const r = document.querySelector('.session-pane:not([hidden]) .rdp-view').getBoundingClientRect(); return [Math.round(r.width * devicePixelRatio), Math.round(r.height * devicePixelRatio)].join('x'); })()`,
);
check('the desktop has the size of the tab', size === viewport, `${size} vs ${viewport}`);
await page.screenshot(`${SHOTS}a3-desktop.png`);

// The mouse: dev_rdpd draws a dot where the left button goes down.
const rect = await page.eval(
  `(() => { const r = ${driver}.canvas.getBoundingClientRect(); return { x: r.left, y: r.top, w: r.width, h: r.height }; })()`,
);
const cx = Math.round(rect.x + rect.w * 0.3);
const cy = Math.round(rect.y + rect.h * 0.6);
const dx = Math.round(((cx - rect.x) / rect.w) * (await page.eval(`${driver}.canvas.width`)));
const dy = Math.round(((cy - rect.y) / rect.h) * (await page.eval(`${driver}.canvas.height`)));
const before = await pixel(dx, dy);
for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
  await page.send('Input.dispatchMouseEvent', {
    type,
    x: cx,
    y: cy,
    button: 'left',
    clickCount: 1,
  });
}
await page.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: cx + 200, y: cy - 150 });
await sleep(800);
const after = await pixel(dx, dy);
check('a click draws on the remote desktop', before !== after, `${before} → ${after}`);

// The keyboard: every key press shifts the background's hue.
const corner = await pixel(5, 5);
await page.eval(`${driver}.focus()`);
await page.send('Input.dispatchKeyEvent', {
  type: 'keyDown',
  key: 'a',
  code: 'KeyA',
  windowsVirtualKeyCode: 65,
  text: 'a',
});
await page.send('Input.dispatchKeyEvent', {
  type: 'keyUp',
  key: 'a',
  code: 'KeyA',
  windowsVirtualKeyCode: 65,
});
await sleep(800);
check('a key press reaches the remote desktop', (await pixel(5, 5)) !== corner);
await page.screenshot(`${SHOTS}a4-input.png`);

// Ctrl+Alt+End is the app's, not the desktop's: it sends Ctrl+Alt+Del.
check(
  'Ctrl+Alt+End is taken by the app',
  await page.eval(`(() => {
    const e = new KeyboardEvent('keydown', { key: 'End', code: 'End', ctrlKey: true, altKey: true, bubbles: true, cancelable: true });
    ${driver}.canvas.dispatchEvent(e);
    return e.defaultPrevented;
  })()`),
);

// ── Resize follows the tab ──────────────────────────────────────────────────
await page.send('Emulation.setDeviceMetricsOverride', {
  width: 1100,
  height: 760,
  deviceScaleFactor: 0,
  mobile: false,
});
await page.waitFor(
  `(() => { const r = document.querySelector('.session-pane:not([hidden]) .rdp-view').getBoundingClientRect(); return Math.abs(${driver}.canvas.width - Math.round(r.width * devicePixelRatio)) <= 2; })()`,
  { what: 'the desktop to follow the tab', timeout: 15_000 },
);
check('the desktop follows a smaller window', true);
await page.send('Emulation.clearDeviceMetricsOverride');
await page.waitFor(
  `(() => { const r = document.querySelector('.session-pane:not([hidden]) .rdp-view').getBoundingClientRect(); return Math.abs(${driver}.canvas.width - Math.round(r.width * devicePixelRatio)) <= 2; })()`,
  { what: 'the desktop to follow the window back', timeout: 15_000 },
);

// ── Overview ────────────────────────────────────────────────────────────────
const sizeBefore = await page.eval(`[${driver}.canvas.width, ${driver}.canvas.height].join('x')`);
await page.click('.host .host-name', 'Übersicht');
await page.waitFor(`document.querySelector('.thumb .thumb-screen')`, { what: 'overview tile' });
check(
  'the overview lists the session without a live picture',
  await page.eval(
    `!document.querySelector('.thumb canvas') && document.querySelector('.thumb').dataset.status === 'live'`,
  ),
);
await page.screenshot(`${SHOTS}a5-overview.png`);
// Longer than the resize delay: a hidden tab must not be asked to change.
await sleep(1_200);
await page.click('.thumb-screen');
await page.waitFor(
  `document.querySelector('.tab[data-active="true"]')?.textContent.includes('dev-rdpd')`,
  {
    what: 'back to the desktop',
  },
);
check('a tile brings its tab to the front', true);
await sleep(1_200);
const sizeAfter = await page.eval(`[${driver}.canvas.width, ${driver}.canvas.height].join('x')`);
check(
  'the overview and back keep the resolution',
  sizeBefore === sizeAfter,
  `${sizeBefore} → ${sizeAfter}`,
);

// ── Disconnect and reconnect ────────────────────────────────────────────────
await page.click('.toolbar-button', 'Trennen');
await page.waitFor(
  `document.querySelector('.session-pane:not([hidden]) .pane-overlay[data-tone="ended"]')`,
  {
    what: 'ended overlay',
    timeout: 15_000,
  },
);
check(
  'a disconnect keeps the tab and its last picture',
  (await page.eval(`document.querySelectorAll('.tab').length`)) >= 1,
);
await page.click('.session-pane:not([hidden]) .pane-overlay button', 'Neu verbinden');
await page.waitFor(`${driver}?.session`, { what: 'reconnected', timeout: 30_000 });
check(
  'a reconnect asks nothing: certificate trusted, login saved',
  !(await page.eval(`!!document.querySelector('.modal')`)),
);

// ── A host nobody answers on ────────────────────────────────────────────────
await page.click('.host .host-name', 'nobody-home');
await page.waitFor(
  `document.querySelector('.session-pane:not([hidden]) .pane-overlay[data-tone="failed"]')`,
  { what: 'a failed connection', timeout: 40_000 },
);
check(
  'an unreachable host fails with a plain reason',
  (await page.text('.notice')).includes('nicht erreichbar'),
  await page.text('.notice'),
);
await page.screenshot(`${SHOTS}a6-unreachable.png`);

// ── A host made by hand, and the form ───────────────────────────────────────
await page.click('.sidebar-head [aria-label="Host hinzufügen"]');
await page.waitFor(`(${topTitle}) === 'Neuer Host'`, { what: 'host form' });
check(
  'a new host defaults to port 3389',
  (await page.eval(`${top}.querySelectorAll('input')[1].value`)) === '3389',
);
await page.eval(`${top}.querySelector('input').focus()`);
await page.type('127.0.0.1');
await page.eval(`${top}.querySelectorAll('input')[1].select()`);
await page.type('3390');
await page.eval(`${top}.querySelectorAll('input')[2].focus()`);
await page.type('by-hand');
await page.screenshot(`${SHOTS}a7-host-form.png`);
await page.click('.modal-footer button', 'Speichern');
await page.waitFor(
  `[...document.querySelectorAll('.host-name')].some(e => e.textContent === 'by-hand')`,
  { what: 'saved host' },
);
check('a host saved by hand appears', true);

// ── Export ──────────────────────────────────────────────────────────────────
await page.click('[aria-label="Einstellungen"]');
await page.waitFor(`document.querySelector('.settings-nav')`, { what: 'settings' });
await page.click('.settings-nav button', 'Import & Export');
await page.click('.setting-row button', 'Exportieren');
await page.waitFor(`(${topTitle}) === 'Exportieren'`, { what: 'export dialog' });
// With passwords, the file gets a password of its own.
if (await page.eval(`${top}.querySelector(".check input").checked`)) {
  await page.eval(`${top}.querySelectorAll("input[type=password]")[0].focus()`);
  await page.type('export-pw-123');
  await page.eval(`${top}.querySelectorAll("input[type=password]")[1].focus()`);
  await page.type('export-pw-123');
}
await page.click('.modal-footer button', 'Speichern unter');
await page.waitFor(`(${topTitle}).startsWith('Export gespeichert')`, {
  what: 'export done',
  timeout: 20_000,
});
const exported = readdirSync(WORK_DIR).filter((name) => name.endsWith('.uwurdp'));
check('the export file was written', exported.length === 1, exported.join(', '));

page.close();
process.exit(failed() ? 1 : 0);
