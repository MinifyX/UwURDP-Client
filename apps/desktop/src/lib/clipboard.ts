/**
 * Files through the clipboard, on the page's side.
 *
 * The engine copies text, pictures and files on its own; the page only shows
 * what it says about files (`CLIPBOARD` messages: a download from the server
 * in progress, files ready to paste here, a set too big to fetch unasked) as
 * a small note over the desktop, and hands files dropped on the desktop to
 * the server's clipboard. Dropped files come from Tauri's own drag-and-drop
 * event: only it knows their real paths.
 */

import { getCurrentWebviewWindow } from '@tauri-apps/api/webviewWindow';
import { language, t } from './i18n';
import { clipboardDownload, clipboardOfferFiles, type SessionId } from './session';

export type ClipboardStatus =
  | { state: 'downloading'; files: number; done: number; total: number }
  | { state: 'ready'; files: number }
  | { state: 'offered'; files: number; total: number | null }
  | { state: 'sent'; files: number }
  | { state: 'failed'; error: 'unsupported' | 'download' | 'offer'; message: string };

/** How long a note stays that needs nothing from the user. */
const NOTE_MS = 4_000;
/** An offer to fetch big files waits a bit longer. */
const OFFER_MS = 15_000;

export function formatSize(bytes: number, lang = language()): string {
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  const digits = unit === 0 || value >= 100 ? 0 : 1;
  const number = new Intl.NumberFormat(lang, { maximumFractionDigits: digits }).format(value);
  return `${number} ${units[unit]}`;
}

/** What a note says, and the button it offers if any. */
export function describe(status: ClipboardStatus): {
  text: string;
  tone: 'info' | 'error';
  action?: string;
} {
  switch (status.state) {
    case 'downloading': {
      const percent = status.total > 0 ? Math.floor((status.done * 100) / status.total) : 0;
      return { text: t('Lade Dateien vom Server… {percent} %', { percent }), tone: 'info' };
    }
    case 'ready':
      return { text: t('Die Dateien vom Server liegen in der Zwischenablage.'), tone: 'info' };
    case 'offered':
      return {
        text:
          status.total === null
            ? t('Der Server bietet Dateien an.')
            : t('Der Server bietet {size} an Dateien an.', { size: formatSize(status.total) }),
        tone: 'info',
        action: t('Laden'),
      };
    case 'sent':
      return { text: t('Mit Strg+V einfügen'), tone: 'info' };
    case 'failed':
      return {
        text:
          status.error === 'unsupported'
            ? t('Der Server nimmt keine Dateien über die Zwischenablage an.')
            : t('Dateien über die Zwischenablage: {message}', { message: status.message }),
        tone: 'error',
      };
  }
}

/** The notes over one desktop, and files dropped on it. */
export class ClipboardNotes {
  private readonly note: HTMLDivElement;
  private hideTimer: number | null = null;
  private unlisten: (() => void) | null = null;
  private disposed = false;

  constructor(
    private readonly container: HTMLElement,
    private readonly canvas: HTMLCanvasElement,
    private readonly session: () => SessionId | null,
  ) {
    this.note = document.createElement('div');
    this.note.className = 'rdp-note';
    this.note.hidden = true;
    this.note.setAttribute('role', 'status');
    container.append(this.note);
    void this.listenForDrops();
  }

  show(status: ClipboardStatus) {
    const { text, tone, action } = describe(status);
    this.note.replaceChildren();
    this.note.dataset.tone = tone;
    const label = document.createElement('span');
    label.textContent = text;
    this.note.append(label);
    if (action) {
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = action;
      button.addEventListener('click', () => {
        const session = this.session();
        if (session) void clipboardDownload(session).catch(() => undefined);
        this.hide();
      });
      this.note.append(button);
    }
    this.note.hidden = false;
    if (this.hideTimer !== null) window.clearTimeout(this.hideTimer);
    this.hideTimer = null;
    // A download in progress stays until it says how it ended.
    if (status.state !== 'downloading') {
      const ms = status.state === 'offered' ? OFFER_MS : NOTE_MS;
      this.hideTimer = window.setTimeout(() => this.hide(), ms);
    }
  }

  private hide() {
    if (this.hideTimer !== null) window.clearTimeout(this.hideTimer);
    this.hideTimer = null;
    this.note.hidden = true;
  }

  dispose() {
    this.disposed = true;
    this.hide();
    this.unlisten?.();
    this.unlisten = null;
    this.note.remove();
  }

  private async listenForDrops() {
    try {
      const unlisten = await getCurrentWebviewWindow().onDragDropEvent(({ payload }) => {
        if (payload.type === 'leave') {
          delete this.container.dataset.drop;
          return;
        }
        const over = this.session() !== null && this.contains(payload.position);
        if (payload.type === 'drop') {
          delete this.container.dataset.drop;
          const session = this.session();
          if (over && session && payload.paths.length > 0) {
            void clipboardOfferFiles(session, payload.paths).catch(() => undefined);
          }
          return;
        }
        if (over) this.container.dataset.drop = 'yes';
        else delete this.container.dataset.drop;
      });
      if (this.disposed) unlisten();
      else this.unlisten = unlisten;
    } catch {
      // Not inside Tauri: nothing can be dropped.
    }
  }

  /** Whether a point of the window (device pixels) is on this desktop. */
  private contains(position: { x: number; y: number }): boolean {
    const scale = window.devicePixelRatio || 1;
    const x = position.x / scale;
    const y = position.y / scale;
    const rect = this.canvas.getBoundingClientRect();
    return (
      rect.width > 0 &&
      rect.height > 0 &&
      x >= rect.left &&
      x <= rect.right &&
      y >= rect.top &&
      y <= rect.bottom
    );
  }
}
