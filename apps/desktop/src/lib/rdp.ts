/**
 * One remote desktop on a canvas, outside React.
 *
 * The engine (`uwurdp-core`) sends binary messages: rectangles of RGBA
 * pixels, the desktop's size, the pointer. This draws them and sends back what
 * the keyboard and mouse do. Pixels are acknowledged message by message, so
 * the engine never has more than two in flight and coalesces everything else
 * into the next one — a slow page makes updates coarser, never late.
 *
 * Message layout, little-endian, first byte the kind:
 *
 * - `1` bitmaps: u16 count, then per rect u16 x, y, w, h and w·h·4 bytes RGBA
 * - `2` desktop size: u16 width, height
 * - `3` pointer bitmap: u16 hot x, hot y, w, h, then w·h·4 bytes RGBA
 * - `4` default pointer, `5` hidden pointer, `6` pointer position u16 x, y
 * - `7` closed: UTF-8 JSON `{reason, message}`
 */

import { scancodeFor } from './keymap';
import {
  ackFrame,
  clipboardChanged,
  closeSession,
  resizeSession,
  sendInput,
  type DataHandler,
  type EndHandler,
  type InputEvent,
  type MouseButton,
  type SessionId,
  type Viewport,
} from './session';

export type Closed = { reason: string; message: string };

/** How the desktop sits in its tab. */
export type Fit = {
  /** The desktop follows the tab's size (the server is asked to resize). */
  follow: boolean;
  /** A desktop that doesn't fit is scaled down instead of scrolled. */
  smartSizing: boolean;
};

const RESIZE_DELAY_MS = 450;
/** One notch of a mouse wheel, in RDP's units. */
const WHEEL_NOTCH = 120;

const BUTTONS: Record<number, MouseButton> = {
  0: 'left',
  1: 'middle',
  2: 'right',
  3: 'x1',
  4: 'x2',
};

export class RdpDriver {
  readonly canvas: HTMLCanvasElement;
  private readonly ctx: CanvasRenderingContext2D;
  private readonly container: HTMLElement;
  session: SessionId | null = null;
  closed: Closed | null = null;
  private fit: Fit;
  private disposed = false;
  private resizeTimer: number | null = null;
  private observer: ResizeObserver;
  private pendingMove: { x: number; y: number } | null = null;
  private moveFrame: number | null = null;
  private wheel = { x: 0, y: 0 };
  private lastSize: { width: number; height: number } | null = null;
  private listeners = new Set<() => void>();
  private cleanup: (() => void)[] = [];
  /** Frames drawn before the session id came back; acknowledged once it does. */
  private earlyAcks = 0;

  constructor(container: HTMLElement, fit: Fit) {
    this.container = container;
    this.fit = fit;
    this.canvas = document.createElement('canvas');
    this.canvas.className = 'rdp-canvas';
    this.canvas.tabIndex = 0;
    this.canvas.width = 1;
    this.canvas.height = 1;
    this.canvas.setAttribute('aria-label', 'Remote desktop');
    container.append(this.canvas);
    const ctx = this.canvas.getContext('2d', { alpha: false, desynchronized: true });
    if (!ctx) throw new Error('no 2D canvas');
    this.ctx = ctx;
    this.ctx.fillStyle = '#0c1030';
    this.ctx.fillRect(0, 0, 1, 1);

    this.observer = new ResizeObserver(() => this.onContainerResize());
    this.observer.observe(container);
    this.bindInput();
  }

  /** Something about the picture changed: the overview redraws its thumbnail. */
  onChange(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** The tab's area in device pixels: the size to ask the server for. */
  viewport(): Viewport {
    const scale = window.devicePixelRatio || 1;
    const rect = this.container.getBoundingClientRect();
    return {
      width: clampSize(Math.round(rect.width * scale)),
      height: clampSize(Math.round(rect.height * scale)),
      scale: Math.round(scale * 100),
    };
  }

  setFit(fit: Fit) {
    this.fit = fit;
    this.layout();
  }

  /**
   * Connect through `spawn`, which hands the engine somewhere to put frames.
   * `onEnd` runs once when the session is over, however it ended.
   */
  async attach(
    spawn: (onData: DataHandler, onEnd: EndHandler) => Promise<SessionId>,
    onEnd: (closed: Closed | null) => void,
  ): Promise<SessionId> {
    this.closed = null;
    this.earlyAcks = 0;
    let ended = false;
    const session = await spawn(
      (bytes) => this.onMessage(bytes),
      () => {
        if (ended) return;
        ended = true;
        this.session = null;
        this.canvas.style.cursor = 'default';
        if (!this.disposed) onEnd(this.closed);
      },
    );
    if (this.disposed) {
      void closeSession(session).catch(() => undefined);
      throw new Error('the tab closed while connecting');
    }
    if (!ended) {
      this.session = session;
      // The engine starts sending before the page hears the session's id.
      for (; this.earlyAcks > 0; this.earlyAcks -= 1) {
        void ackFrame(session).catch(() => undefined);
      }
    }
    return session;
  }

  focus() {
    this.canvas.focus({ preventScroll: true });
  }

  send(events: InputEvent[]) {
    if (this.session && events.length > 0) {
      void sendInput(this.session, events).catch(() => undefined);
    }
  }

  ctrlAltDel() {
    this.send([{ type: 'ctrlAltDel' }]);
    this.focus();
  }

  /** Ends the session and lets go of the canvas. */
  dispose() {
    if (this.disposed) return;
    this.disposed = true;
    this.observer.disconnect();
    for (const undo of this.cleanup) undo();
    if (this.resizeTimer !== null) window.clearTimeout(this.resizeTimer);
    if (this.moveFrame !== null) window.cancelAnimationFrame(this.moveFrame);
    if (this.session) void closeSession(this.session).catch(() => undefined);
    this.session = null;
    this.canvas.remove();
    this.listeners.clear();
  }

  /** Disconnects but keeps the last picture, for a tab that stays open. */
  disconnect() {
    if (this.session) void closeSession(this.session).catch(() => undefined);
  }

  // ── Drawing ──────────────────────────────────────────────────────────────

  private onMessage(bytes: Uint8Array) {
    const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
    const kind = bytes[0];
    try {
      switch (kind) {
        case 1: {
          const count = view.getUint16(1, true);
          let at = 3;
          for (let i = 0; i < count; i += 1) {
            const x = view.getUint16(at, true);
            const y = view.getUint16(at + 2, true);
            const w = view.getUint16(at + 4, true);
            const h = view.getUint16(at + 6, true);
            at += 8;
            const length = w * h * 4;
            if (w > 0 && h > 0 && at + length <= bytes.byteLength) {
              const pixels = new Uint8ClampedArray(
                bytes.buffer as ArrayBuffer,
                bytes.byteOffset + at,
                length,
              );
              this.ctx.putImageData(new ImageData(pixels, w, h), x, y);
            }
            at += length;
          }
          // Drawn: the engine may send the next one.
          this.ack();
          for (const listener of this.listeners) listener();
          break;
        }
        case 2: {
          const width = view.getUint16(1, true);
          const height = view.getUint16(3, true);
          if (width > 0 && height > 0) {
            this.canvas.width = width;
            this.canvas.height = height;
            this.lastSize = { width, height };
            this.layout();
          }
          break;
        }
        case 3: {
          const hotX = view.getUint16(1, true);
          const hotY = view.getUint16(3, true);
          const w = view.getUint16(5, true);
          const h = view.getUint16(7, true);
          if (w > 0 && h > 0 && 9 + w * h * 4 <= bytes.byteLength) {
            this.canvas.style.cursor = pointerCss(
              bytes.subarray(9, 9 + w * h * 4),
              w,
              h,
              hotX,
              hotY,
            );
          }
          break;
        }
        case 4:
          this.canvas.style.cursor = 'default';
          break;
        case 5:
          this.canvas.style.cursor = 'none';
          break;
        case 6:
          // The server moved the pointer; a page can't move the real one.
          break;
        case 7: {
          const text = new TextDecoder().decode(bytes.subarray(1));
          const parsed = JSON.parse(text) as Partial<Closed>;
          this.closed = {
            reason: String(parsed.reason ?? 'disconnect'),
            message: String(parsed.message ?? ''),
          };
          break;
        }
      }
    } catch {
      // A damaged message is dropped; the next full update repairs the picture.
      if (kind === 1) this.ack();
    }
  }

  private ack() {
    if (this.session) void ackFrame(this.session).catch(() => undefined);
    else this.earlyAcks += 1;
  }

  /** Puts the canvas where it belongs: 1:1, scaled down, or scrollable. */
  private layout() {
    const scale = window.devicePixelRatio || 1;
    const rect = this.container.getBoundingClientRect();
    const width = this.canvas.width / scale;
    const height = this.canvas.height / scale;
    let cssWidth = width;
    let cssHeight = height;
    if (this.fit.smartSizing && rect.width > 0 && rect.height > 0) {
      const factor = Math.min(1, rect.width / width, rect.height / height);
      cssWidth = width * factor;
      cssHeight = height * factor;
    }
    this.canvas.style.width = `${cssWidth}px`;
    this.canvas.style.height = `${cssHeight}px`;
    this.container.dataset.scroll = this.fit.smartSizing ? 'no' : 'yes';
  }

  private onContainerResize() {
    this.layout();
    if (!this.fit.follow || !this.session) return;
    if (this.resizeTimer !== null) window.clearTimeout(this.resizeTimer);
    this.resizeTimer = window.setTimeout(() => {
      this.resizeTimer = null;
      const { width, height, scale } = this.viewport();
      // Hidden tabs measure as zero: they keep the size they have.
      if (width < 200 || height < 200 || !this.session) return;
      if (this.lastSize && this.lastSize.width === width && this.lastSize.height === height) return;
      void resizeSession(this.session, width, height, scale).catch(() => undefined);
    }, RESIZE_DELAY_MS);
  }

  // ── Input ────────────────────────────────────────────────────────────────

  /** A point on the canvas, in desktop pixels. */
  private point(event: { clientX: number; clientY: number }) {
    const rect = this.canvas.getBoundingClientRect();
    const x = ((event.clientX - rect.left) * this.canvas.width) / Math.max(rect.width, 1);
    const y = ((event.clientY - rect.top) * this.canvas.height) / Math.max(rect.height, 1);
    return {
      x: Math.max(0, Math.min(this.canvas.width - 1, Math.round(x))),
      y: Math.max(0, Math.min(this.canvas.height - 1, Math.round(y))),
    };
  }

  private listen<K extends keyof HTMLElementEventMap>(
    type: K,
    handler: (event: HTMLElementEventMap[K]) => void,
    options?: AddEventListenerOptions,
  ) {
    this.canvas.addEventListener(type, handler as EventListener, options);
    this.cleanup.push(() =>
      this.canvas.removeEventListener(type, handler as EventListener, options),
    );
  }

  private bindInput() {
    this.listen('keydown', (event) => this.key(event, true));
    this.listen('keyup', (event) => this.key(event, false));

    this.listen('pointermove', (event) => {
      this.pendingMove = this.point(event);
      if (this.moveFrame !== null) return;
      // One move per frame is all a remote desktop can show anyway.
      this.moveFrame = window.requestAnimationFrame(() => {
        this.moveFrame = null;
        if (this.pendingMove) this.send([{ type: 'move', ...this.pendingMove }]);
        this.pendingMove = null;
      });
    });
    this.listen('pointerdown', (event) => {
      const button = BUTTONS[event.button];
      if (!button) return;
      event.preventDefault();
      this.focus();
      this.canvas.setPointerCapture(event.pointerId);
      this.flushMove();
      this.send([{ type: 'button', button, down: true, ...this.point(event) }]);
    });
    this.listen('pointerup', (event) => {
      const button = BUTTONS[event.button];
      if (!button) return;
      event.preventDefault();
      this.flushMove();
      this.send([{ type: 'button', button, down: false, ...this.point(event) }]);
    });
    this.listen('contextmenu', (event) => event.preventDefault());
    // The browser's own back/forward on the side buttons must not fire.
    this.listen('auxclick', (event) => event.preventDefault());
    this.listen(
      'wheel',
      (event) => {
        event.preventDefault();
        this.flushMove();
        const unit = event.deltaMode === 1 ? 40 : event.deltaMode === 2 ? 800 : 1;
        this.wheel.y += -event.deltaY * unit * 1.2;
        this.wheel.x += event.deltaX * unit * 1.2;
        const events: InputEvent[] = [];
        for (const axis of ['y', 'x'] as const) {
          while (Math.abs(this.wheel[axis]) >= WHEEL_NOTCH) {
            const step = Math.sign(this.wheel[axis]) * WHEEL_NOTCH;
            this.wheel[axis] -= step;
            events.push({ type: 'wheel', delta: step, horizontal: axis === 'x' });
          }
        }
        this.send(events);
      },
      { passive: false },
    );
    this.listen('blur', () => {
      // Whatever was held when the focus left must not stay pressed remotely.
      this.send([{ type: 'releaseAll' }]);
    });
    this.listen('focus', () => {
      if (this.session) void clipboardChanged(this.session).catch(() => undefined);
    });
  }

  private flushMove() {
    if (this.moveFrame !== null) {
      window.cancelAnimationFrame(this.moveFrame);
      this.moveFrame = null;
    }
    if (this.pendingMove) this.send([{ type: 'move', ...this.pendingMove }]);
    this.pendingMove = null;
  }

  private key(event: KeyboardEvent, down: boolean) {
    if (event.isComposing) return;
    const scancode = scancodeFor(event.code);
    if (scancode) {
      event.preventDefault();
      event.stopPropagation();
      this.send([{ type: 'key', code: scancode.code, extended: scancode.extended, down }]);
      return;
    }
    if (event.key.length > 0 && [...event.key].length === 1) {
      event.preventDefault();
      event.stopPropagation();
      this.send([{ type: 'unicode', ch: event.key, down }]);
    }
  }
}

function clampSize(value: number): number {
  return Math.max(200, Math.min(8192, value));
}

/** A pointer bitmap as a CSS cursor. */
function pointerCss(rgba: Uint8Array, w: number, h: number, hotX: number, hotY: number): string {
  const canvas = document.createElement('canvas');
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext('2d');
  if (!ctx) return 'default';
  ctx.putImageData(new ImageData(new Uint8ClampedArray(rgba), w, h), 0, 0);
  const x = Math.min(hotX, w - 1);
  const y = Math.min(hotY, h - 1);
  return `url(${canvas.toDataURL('image/png')}) ${x} ${y}, default`;
}
