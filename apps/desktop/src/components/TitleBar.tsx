import { getCurrentWindow } from '@tauri-apps/api/window';
import { useEffect, useState, type ReactNode } from 'react';
import { t, useLanguage } from '../lib/i18n';
import { Nyu } from './nyu/Nyu';

type Props = {
  onSettings: () => void;
  children?: ReactNode;
};

const ICONS = {
  settings:
    'M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6Z M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1Z',
};

/**
 * The window's own title bar: the window has no system frame (see
 * tauri.conf.json), so moving, minimizing, maximizing and closing all happen
 * here. Double-clicking the empty bar maximizes, as everywhere on Windows.
 */
export function TitleBar({ onSettings, children }: Props) {
  useLanguage();
  const [maximized, setMaximized] = useState(false);

  useEffect(() => {
    const window = getCurrentWindow();
    let stopped = false;
    let unlisten: (() => void) | undefined;
    const sync = () =>
      void window
        .isMaximized()
        .then((value) => !stopped && setMaximized(value))
        .catch(() => undefined);
    sync();
    void window
      .onResized(sync)
      .then((stop) => {
        if (stopped) stop();
        else unlisten = stop;
      })
      .catch(() => undefined);
    return () => {
      stopped = true;
      unlisten?.();
    };
  }, []);

  const window = () => getCurrentWindow();

  return (
    <header className="titlebar" data-tauri-drag-region>
      <span className="titlebar-brand" data-tauri-drag-region>
        <Nyu size={22} blink={false} title="UwURDP" />
        <span className="wordmark" data-tauri-drag-region>
          <span>UwU</span>RDP
        </span>
      </span>
      <span className="spacer" data-tauri-drag-region />
      {children}
      <button
        className="titlebar-action"
        onClick={onSettings}
        title={t('Einstellungen (Strg+,)')}
        aria-label={t('Einstellungen')}
      >
        <svg viewBox="0 0 24 24" aria-hidden>
          <path d={ICONS.settings} />
        </svg>
      </button>
      <div className="window-controls">
        <button
          className="window-control"
          onClick={() => void window().minimize()}
          title={t('Minimieren')}
          aria-label={t('Minimieren')}
        >
          <svg viewBox="0 0 10 10" aria-hidden>
            <path d="M0 5.5h10" />
          </svg>
        </button>
        <button
          className="window-control"
          onClick={() => void window().toggleMaximize()}
          title={maximized ? t('Verkleinern') : t('Maximieren')}
          aria-label={maximized ? t('Verkleinern') : t('Maximieren')}
        >
          {maximized ? (
            <svg viewBox="0 0 10 10" aria-hidden>
              <path d="M2.5 2.5V.5h7v7h-2 M.5 2.5h7v7h-7z" />
            </svg>
          ) : (
            <svg viewBox="0 0 10 10" aria-hidden>
              <path d="M.5.5h9v9h-9z" />
            </svg>
          )}
        </button>
        <button
          className="window-control close"
          onClick={() => void window().close()}
          title={t('Schließen')}
          aria-label={t('Schließen')}
        >
          <svg viewBox="0 0 10 10" aria-hidden>
            <path d="M.5.5l9 9 M9.5.5l-9 9" />
          </svg>
        </button>
      </div>
    </header>
  );
}
