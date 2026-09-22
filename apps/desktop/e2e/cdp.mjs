// A minimal Chrome DevTools Protocol driver for the UwURDP webview.
// Real input events (mouse, keyboard, text insertion), not synthetic DOM
// events, so React and xterm.js see exactly what a user would produce.

import { writeFileSync } from 'node:fs';

const PORT = 9223;

/** The app behind a DevTools port: 9223 for the one `pnpm tauri dev` starts. */
export async function connect(port = PORT) {
  let target;
  for (let i = 0; i < 60 && !target; i++) {
    try {
      const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
      target = list.find((t) => t.type === 'page' && t.url.startsWith('http://localhost:1420'));
    } catch {}
    if (!target) await sleep(500);
  }
  if (!target) throw new Error('no UwURDP page on the debugging port');

  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((resolve, reject) => {
    ws.onopen = resolve;
    ws.onerror = reject;
  });

  let nextId = 1;
  const pending = new Map();
  ws.onmessage = (event) => {
    const message = JSON.parse(event.data);
    if (message.id && pending.has(message.id)) {
      const { resolve, reject } = pending.get(message.id);
      pending.delete(message.id);
      message.error ? reject(new Error(JSON.stringify(message.error))) : resolve(message.result);
    }
  };

  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = nextId++;
      pending.set(id, { resolve, reject });
      ws.send(JSON.stringify({ id, method, params }));
    });

  const page = {
    send,
    close: () => ws.close(),

    async eval(expression) {
      const result = await send('Runtime.evaluate', {
        expression,
        awaitPromise: true,
        returnByValue: true,
      });
      if (result.exceptionDetails) {
        throw new Error(
          `eval failed: ${result.exceptionDetails.exception?.description ?? expression}`,
        );
      }
      return result.result.value;
    },

    async waitFor(expression, { timeout = 10_000, what = expression } = {}) {
      const started = Date.now();
      for (;;) {
        // Coerce in the page: DOM nodes and the driver cannot be returned by
        // value ("Object reference chain is too long").
        const value = await page.eval(`!!(${expression})`).catch(() => undefined);
        if (value) return value;
        if (Date.now() - started > timeout) throw new Error(`timed out waiting for: ${what}`);
        await sleep(100);
      }
    },

    /** Centre of the first element matching `selector` whose text includes `text`. */
    async locate(selector, text) {
      const point = await page.eval(`(() => {
        const els = [...document.querySelectorAll(${JSON.stringify(selector)})]
          .filter((el) => ${text === undefined ? 'true' : `el.textContent.includes(${JSON.stringify(text)})`});
        const el = els[0];
        if (!el) return null;
        el.scrollIntoView({ block: 'nearest' });
        const r = el.getBoundingClientRect();
        return { x: r.left + r.width / 2, y: r.top + r.height / 2, disabled: !!el.disabled };
      })()`);
      if (!point) throw new Error(`nothing matches ${selector}${text ? ` with "${text}"` : ''}`);
      return point;
    },

    async click(selector, text) {
      const { x, y, disabled } = await page.locate(selector, text);
      if (disabled) throw new Error(`${selector} "${text ?? ''}" is disabled`);
      for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
        await send('Input.dispatchMouseEvent', { type, x, y, button: 'left', clickCount: 1 });
      }
      await sleep(80);
    },

    async rightClick(selector, text) {
      const { x, y } = await page.locate(selector, text);
      for (const type of ['mouseMoved', 'mousePressed', 'mouseReleased']) {
        await send('Input.dispatchMouseEvent', { type, x, y, button: 'right', clickCount: 1 });
      }
      await sleep(80);
    },

    /**
     * A new terminal tab to a host from the sidebar, whether or not one is
     * open already: a plain click only brings an open one to the front.
     */
    async openHost(name) {
      await page.rightClick('.host .host-name', name);
      await page.waitFor(`document.querySelector('.context-menu')`, { what: 'host menu' });
      const another = await page.eval(
        `[...document.querySelectorAll('.context-menu [role=menuitem]')].some(e => e.textContent.includes('Weiteren Tab öffnen'))`,
      );
      await page.click(
        '.context-menu [role=menuitem]',
        another ? 'Weiteren Tab öffnen' : 'Verbinden',
      );
    },

    async type(text) {
      await send('Input.insertText', { text });
      await sleep(40);
    },

    async key(key, modifiers = 0) {
      const codes = {
        Enter: { code: 'Enter', vk: 13, text: '\r' },
        Escape: { code: 'Escape', vk: 27 },
        Backspace: { code: 'Backspace', vk: 8 },
        Tab: { code: 'Tab', vk: 9 },
        a: { code: 'KeyA', vk: 65 },
      };
      const k = codes[key];
      const base = { key, code: k.code, windowsVirtualKeyCode: k.vk, modifiers };
      await send('Input.dispatchKeyEvent', {
        type: 'keyDown',
        ...base,
        ...(k.text && !modifiers ? { text: k.text } : {}),
      });
      await send('Input.dispatchKeyEvent', { type: 'keyUp', ...base });
      await sleep(60);
    },

    /** Replace the content of the input under `selector`. */
    async fill(selector, text) {
      await page.click(selector);
      await page.key('a', 2 /* Ctrl */);
      await page.key('Backspace');
      if (text) await page.type(text);
    },

    async screenshot(path) {
      const { data } = await send('Page.captureScreenshot', { format: 'png' });
      writeFileSync(path, Buffer.from(data, 'base64'));
      return path;
    },

    /** The visible terminal, as text. */
    async terminal() {
      return page.eval(`(() => {
        const term = window.__uwurdpDriver?.term;
        if (!term) return '';
        const buf = term.buffer.active;
        const lines = [];
        for (let i = 0; i < buf.length; i++) lines.push(buf.getLine(i)?.translateToString(true) ?? '');
        return lines.join('\\n');
      })()`);
    },

    async text(selector) {
      return page.eval(
        `[...document.querySelectorAll(${JSON.stringify(selector)})].map((e) => e.textContent).join(' | ')`,
      );
    },
  };

  return page;
}

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

let failures = 0;
export function check(label, ok, detail = '') {
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}${detail ? `  — ${detail}` : ''}`);
  if (!ok) failures += 1;
}
export const failed = () => failures;
