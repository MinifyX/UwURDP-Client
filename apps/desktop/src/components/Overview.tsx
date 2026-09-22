import { useEffect, useRef } from 'react';
import { t, useLanguage } from '../lib/i18n';
import type { RdpDriver } from '../lib/rdp';
import type { HostRecord } from '../lib/session';
import { useSettings } from '../lib/settings';
import { hostLine, type Tab } from '../lib/tabs';
import { Icon } from './Icon';
import { NyuScene } from './nyu/scenes';

export type OverviewEntry = {
  host: HostRecord;
  /** The open tab to this host, if there is one. */
  tab: Tab | null;
  driver: RdpDriver | null;
};

type Props = {
  entries: OverviewEntry[];
  /** Shows a group rather than every open session: hosts without a tab appear too. */
  group: string | null;
  onShow: (tabId: string) => void;
  onConnect: (host: HostRecord) => void;
  onConnectAll: () => void;
  onDisconnect: (tabId: string) => void;
};

const WIDTHS = { small: 200, medium: 300, large: 440 } as const;
/** Thumbnails redraw at most this often; a live desktop changes constantly. */
const REDRAW_MS = 500;

/** One desktop, drawn small from its session's canvas. */
function Thumbnail({ driver, width }: { driver: RdpDriver; width: number }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const canvas = ref.current;
    if (!canvas) return;
    let last = 0;
    let timer: number | null = null;
    const draw = () => {
      timer = null;
      last = Date.now();
      const source = driver.canvas;
      const ratio = source.height / Math.max(source.width, 1);
      const scale = window.devicePixelRatio || 1;
      canvas.width = Math.round(width * scale);
      canvas.height = Math.round(width * ratio * scale);
      const ctx = canvas.getContext('2d');
      if (!ctx) return;
      ctx.imageSmoothingQuality = 'medium';
      ctx.drawImage(source, 0, 0, canvas.width, canvas.height);
    };
    const schedule = () => {
      if (timer !== null) return;
      timer = window.setTimeout(draw, Math.max(0, REDRAW_MS - (Date.now() - last)));
    };
    draw();
    const stop = driver.onChange(schedule);
    return () => {
      stop();
      if (timer !== null) window.clearTimeout(timer);
    };
  }, [driver, width]);
  return <canvas ref={ref} className="thumb-canvas" style={{ width }} aria-hidden />;
}

/**
 * RDCMan's group view: every desktop at a glance, live. A click brings one to
 * the front; hosts of the group that aren't connected get a button instead.
 */
export function Overview({ entries, group, onShow, onConnect, onConnectAll, onDisconnect }: Props) {
  useLanguage();
  const settings = useSettings();
  const width = WIDTHS[settings.thumbnailSize];
  const idle = entries.filter((entry) => !entry.tab);

  if (entries.length === 0) {
    return (
      <div className="no-tabs">
        <NyuScene name="sleepy" className="no-tabs-scene" />
        <p className="no-tabs-title">
          {group ? t('In dieser Gruppe sind keine Hosts.') : t('Keine Sitzung offen')}
        </p>
        <p className="no-tabs-text">
          {t('Verbundene Desktops erscheinen hier als Vorschau – klick eine an, um hinzuwechseln.')}
        </p>
      </div>
    );
  }

  return (
    <div className="overview">
      {group && idle.length > 0 && (
        <div className="overview-head">
          <span>
            {t('{count} von {total} verbunden', {
              count: entries.length - idle.length,
              total: entries.length,
            })}
          </span>
          <span className="spacer" />
          <button onClick={onConnectAll}>
            <Icon name="power" size={15} />
            {t('Alle verbinden')}
          </button>
        </div>
      )}
      <ul className="thumbs" style={{ gridTemplateColumns: `repeat(auto-fill, ${width}px)` }}>
        {entries.map(({ host, tab, driver }) => (
          <li key={tab?.id ?? host.id} className="thumb" data-status={tab?.status ?? 'idle'}>
            <button
              className="thumb-screen"
              style={{ width, minHeight: Math.round((width * 9) / 16) }}
              onClick={() => (tab ? onShow(tab.id) : onConnect(host))}
              title={tab ? t('Zu {name} wechseln', { name: host.name }) : t('Verbinden')}
            >
              {tab && driver && tab.status !== 'failed' ? (
                <Thumbnail driver={driver} width={width} />
              ) : (
                <span className="thumb-idle">
                  <Icon name={tab?.status === 'failed' ? 'close' : 'power'} size={22} />
                  <span>{tab?.status === 'failed' ? t('Nicht verbunden') : t('Verbinden')}</span>
                </span>
              )}
              {tab?.status === 'connecting' && (
                <span className="thumb-badge">{t('verbindet…')}</span>
              )}
            </button>
            <div className="thumb-caption">
              <i
                className="dot"
                data-state={
                  tab?.status === 'live'
                    ? 'online'
                    : tab?.status === 'connecting'
                      ? 'connecting'
                      : 'idle'
                }
              />
              <span className="thumb-text">
                <b>{host.name}</b>
                <small>{hostLine(host)}</small>
              </span>
              {tab && tab.status === 'live' && (
                <button
                  className="icon-button"
                  onClick={() => onDisconnect(tab.id)}
                  title={t('Trennen')}
                  aria-label={t('{name} trennen', { name: host.name })}
                >
                  <Icon name="close" size={14} />
                </button>
              )}
            </div>
          </li>
        ))}
      </ul>
    </div>
  );
}
