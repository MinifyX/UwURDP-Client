import { t, useLanguage } from '../lib/i18n';
import type { Tab } from '../lib/tabs';
import { Icon } from './Icon';

type Props = {
  tabs: Tab[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onClose: (id: string) => void;
  onOverview: () => void;
};

function stateOf(tab: Tab): 'online' | 'connecting' | 'idle' {
  if (tab.status === 'connecting') return 'connecting';
  if (tab.status === 'live') return 'online';
  return 'idle';
}

/**
 * One tab per session. Every click on a host opens another one, so several
 * connections to the same server sit side by side; the number after a repeated
 * name tells them apart. Middle click closes, like in a browser.
 */
export function TabBar({ tabs, activeId, onSelect, onClose, onOverview }: Props) {
  useLanguage();
  return (
    <div className="tabbar">
      <div className="tabs" role="tablist" aria-label={t('Offene Sitzungen')}>
        {tabs.map((tab, index) => {
          const active = tab.id === activeId;
          const name = tab.subtitle ? `${tab.title} · ${tab.subtitle}` : tab.title;
          return (
            <div
              key={tab.id}
              className="tab"
              data-active={active}
              data-status={tab.status}
              onMouseDown={(event) => {
                // Middle click closes without selecting first.
                if (event.button === 1) {
                  event.preventDefault();
                  onClose(tab.id);
                }
              }}
            >
              <button
                role="tab"
                className="tab-select"
                aria-selected={active}
                title={
                  index < 9
                    ? t('{name} (Strg+Umschalt+{number})', { name, number: index + 1 })
                    : name
                }
                onClick={() => onSelect(tab.id)}
              >
                <span className="tab-icon" aria-hidden>
                  <Icon name={tab.kind === 'overview' ? 'grid' : 'monitor'} size={15} />
                  <i className="dot" data-state={stateOf(tab)} />
                </span>
                <span className="tab-title">{tab.title}</span>
                {tab.ordinal > 1 && <span className="tab-ordinal">{tab.ordinal}</span>}
              </button>
              <button
                className="tab-close"
                onClick={() => onClose(tab.id)}
                title={t('Tab schließen (Strg+Umschalt+W)')}
                aria-label={t('{name} schließen', { name: tab.title })}
              >
                ×
              </button>
            </div>
          );
        })}
      </div>
      <button
        className="icon-button tab-new"
        onClick={onOverview}
        title={t('Übersicht aller Sitzungen (Strg+Umschalt+O)')}
        aria-label={t('Übersicht')}
      >
        <Icon name="grid" size={15} />
      </button>
    </div>
  );
}
