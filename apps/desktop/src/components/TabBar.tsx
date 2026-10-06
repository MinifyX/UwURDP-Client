import { Icon, IconButton, ICONS } from '@uwusuite/design';
import { t, useLanguage } from '../lib/i18n';
import { SHORTCUTS, withKeys } from '../lib/shortcuts';
import type { Tab } from '../lib/tabs';

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
 *
 * The suite's tab (package docs/components.md): a device icon with a state
 * dot, the title, a close button on hover, a pink top inset when active.
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
                title={index < 9 ? withKeys(name, `CmdOrCtrl+Shift+${index + 1}`) : name}
                onClick={() => onSelect(tab.id)}
              >
                <span className="tab-icon" aria-hidden>
                  <Icon icon={tab.kind === 'overview' ? ICONS.overview : ICONS.computer} />
                  <i className="dot" data-state={stateOf(tab)} />
                </span>
                <span className="tab-title">{tab.title}</span>
                {tab.ordinal > 1 && <span className="tab-ordinal">{tab.ordinal}</span>}
              </button>
              <button
                type="button"
                className="tab-close"
                onClick={() => onClose(tab.id)}
                title={withKeys(t('Tab schließen'), SHORTCUTS.closeTab)}
                aria-label={t('{name} schließen', { name: tab.title })}
              >
                <Icon icon={ICONS.close} size="xs" />
              </button>
            </div>
          );
        })}
      </div>
      <IconButton
        size="sm"
        icon={ICONS.overview}
        className="tab-new"
        onClick={onOverview}
        label={withKeys(t('Übersicht aller Sitzungen'), SHORTCUTS.overview)}
      />
    </div>
  );
}
