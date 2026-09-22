import { t, useLanguage } from '../lib/i18n';
import type { HostRecord } from '../lib/session';
import { hostLine, type Tab } from '../lib/tabs';
import { Icon } from './Icon';
import { NyuScene } from './nyu/scenes';

export type OverviewEntry = {
  host: HostRecord;
  /** The open tab to this host, if there is one. */
  tab: Tab | null;
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

/**
 * RDCMan's group view: every desktop of a group at a glance. A click brings
 * one to the front; hosts that aren't connected get a button instead. No live
 * pictures: they cost every session time and say little a name doesn't.
 */
export function Overview({ entries, group, onShow, onConnect, onConnectAll, onDisconnect }: Props) {
  useLanguage();
  const idle = entries.filter((entry) => !entry.tab);

  if (entries.length === 0) {
    return (
      <div className="no-tabs">
        <NyuScene name="sleepy" className="no-tabs-scene" />
        <p className="no-tabs-title">
          {group ? t('In dieser Gruppe sind keine Hosts.') : t('Keine Sitzung offen')}
        </p>
        <p className="no-tabs-text">
          {t('Verbundene Desktops erscheinen hier – klick einen an, um hinzuwechseln.')}
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
      <ul className="thumbs">
        {entries.map(({ host, tab }) => {
          const failed = tab?.status === 'failed';
          const live = tab?.status === 'live';
          return (
            <li key={tab?.id ?? host.id} className="thumb" data-status={tab?.status ?? 'idle'}>
              <button
                className="thumb-screen"
                onClick={() => (tab ? onShow(tab.id) : onConnect(host))}
                title={tab ? t('Zu {name} wechseln', { name: host.name }) : t('Verbinden')}
              >
                <span className="thumb-idle">
                  <Icon name={failed ? 'close' : tab ? 'monitor' : 'power'} size={22} />
                  <span>
                    {failed
                      ? t('Nicht verbunden')
                      : live
                        ? t('Zu {name} wechseln', { name: host.name })
                        : tab?.status === 'connecting'
                          ? t('verbindet…')
                          : tab
                            ? t('Getrennt.')
                            : t('Verbinden')}
                  </span>
                </span>
              </button>
              <div className="thumb-caption">
                <i
                  className="dot"
                  data-state={
                    live ? 'online' : tab?.status === 'connecting' ? 'connecting' : 'idle'
                  }
                />
                <span className="thumb-text">
                  <b>{host.name}</b>
                  <small>{hostLine(host)}</small>
                </span>
                {tab && live && (
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
          );
        })}
      </ul>
    </div>
  );
}
