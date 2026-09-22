import { useMemo, useState, type KeyboardEvent as ReactKeyboardEvent } from 'react';
import { beginDrag, edgeByHalf, type DropTarget } from '../lib/dnd';
import { t } from '../lib/i18n';
import {
  createGroup,
  deleteGroup,
  moveGroup,
  moveHost,
  renameGroup,
  type GroupRecord,
  type HostRecord,
  type Workspace,
} from '../lib/session';
import { updateSettings, useSettings, workspaceName } from '../lib/settings';
import { hostLine } from '../lib/tabs';
import { ContextMenu, type MenuItem } from './ContextMenu';
import { Icon } from './Icon';
import { Nyu } from './nyu/Nyu';

type Props = {
  hosts: HostRecord[];
  groups: GroupRecord[];
  /** `'overview'` for the overview of all sessions, a host id, or nothing: what the active tab shows. */
  activeId: string | null;
  /** Hosts with at least one live session in some tab. */
  onlineIds: ReadonlySet<string>;
  /** Hosts a tab is connecting to right now. */
  connectingIds: ReadonlySet<string>;
  /** Hosts with a tab open, live or not. */
  openIds: ReadonlySet<string>;
  /** Brings the host's open tab to the front, or opens one. */
  onConnect: (host: HostRecord) => void;
  /** Always a new tab, next to the ones already open. */
  onConnectAnother: (host: HostRecord) => void;
  /** Closes every tab to the host. */
  onDisconnect: (host: HostRecord) => void;
  /** Thumbnails: of every open session, or of one group's hosts. */
  onOverview: (workspace: Workspace | null, group: string | null) => void;
  onConnectGroup: (workspace: Workspace, group: string) => void;
  onDisconnectGroup: (workspace: Workspace, group: string) => void;
  onGroupLogin: (group: GroupRecord) => void;
  onAdd: (workspace: Workspace, group: string | null) => void;
  onEdit: (host: HostRecord) => void;
  onImport: () => void;
  /** Something moved or changed: load hosts and groups again. */
  onChanged: () => void;
  onError: (text: string) => void;
};

const WORKSPACES: Workspace[] = ['private', 'business'];

type Section = { name: string | null; hosts: HostRecord[] };

function byPlace(a: HostRecord, b: HostRecord) {
  return a.position - b.position || a.name.localeCompare(b.name, 'de');
}

/** The ungrouped hosts, then every group in its order, for one workspace. */
function sectionsOf(hosts: HostRecord[], groups: GroupRecord[], workspace: Workspace): Section[] {
  const mine = hosts.filter((host) => host.workspace === workspace);
  const names = [...new Set(groups.filter((g) => g.workspace === workspace).map((g) => g.name))];
  for (const host of mine) {
    if (host.groupPath && !names.includes(host.groupPath)) names.push(host.groupPath);
  }
  return [
    { name: null, hosts: mine.filter((h) => !h.groupPath).sort(byPlace) },
    ...names.map((name) => ({
      name,
      hosts: mine.filter((h) => h.groupPath === name).sort(byPlace),
    })),
  ];
}

function matches(host: HostRecord, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (!q) return true;
  return [
    host.name,
    host.address,
    host.username,
    host.domain,
    host.groupPath ?? '',
    host.comment,
  ].some((field) => field.toLowerCase().includes(q));
}

function errorText(error: unknown): string {
  if (typeof error === 'object' && error !== null && 'kind' in error) {
    const failure = error as { kind: string; problem?: string; message?: string };
    if (failure.problem === 'exists') return t('Eine Gruppe mit diesem Namen gibt es schon.');
    if (failure.problem === 'required') return t('Die Gruppe braucht einen Namen.');
    if (failure.message) return failure.message;
  }
  return String(error);
}

export function HostList(props: Props) {
  const { hosts, groups, activeId, onlineIds, connectingIds, openIds } = props;
  const settings = useSettings();
  const workspace: Workspace = settings.workspaces ? settings.activeWorkspace : 'private';
  const [query, setQuery] = useState('');
  const [menu, setMenu] = useState<{ x: number; y: number; items: MenuItem[] } | null>(null);
  const [editing, setEditing] = useState<{ from: string | null; value: string } | null>(null);
  const [dragging, setDragging] = useState(false);

  // With workspaces off, everything shows as one list.
  const sections = useMemo(
    () =>
      settings.workspaces
        ? sectionsOf(hosts, groups, workspace)
        : sectionsOf(
            hosts.map((host) => ({ ...host, workspace: 'private' as const })),
            groups.map((group) => ({ ...group, workspace: 'private' as const })),
            'private',
          ),
    [hosts, groups, workspace, settings.workspaces],
  );
  const found = useMemo(
    () => (query.trim() ? hosts.filter((host) => matches(host, query)) : []),
    [hosts, query],
  );
  const counts = useMemo(() => {
    const count: Record<Workspace, number> = { private: 0, business: 0 };
    for (const host of hosts) count[host.workspace] += 1;
    return count;
  }, [hosts]);

  const collapsed = (name: string) => settings.collapsedGroups.includes(`${workspace}/${name}`);
  const toggle = (name: string) => {
    const key = `${workspace}/${name}`;
    updateSettings({
      collapsedGroups: collapsed(name)
        ? settings.collapsedGroups.filter((g) => g !== key)
        : [...settings.collapsedGroups, key],
    });
  };

  const run = (action: () => Promise<unknown>) =>
    void action()
      .then(props.onChanged)
      .catch((error) => props.onError(errorText(error)));

  // ── Dragging ──────────────────────────────────────────────────────────────

  const dragHost = (event: React.PointerEvent, host: HostRecord) => {
    beginDrag(event, {
      label: host.name,
      onStart: () => setDragging(true),
      onEnd: () => setDragging(false),
      accept: (element, y) => {
        const kind = element.dataset.drop;
        if (kind === 'host')
          return element.dataset.hostId === host.id ? null : edgeByHalf(element, y);
        if (kind === 'group' || kind === 'workspace') return 'inside';
        return null;
      },
      onDrop: (target: DropTarget) => {
        const data = target.data;
        const to = settings.workspaces
          ? ((data.workspace as Workspace) ?? workspace)
          : host.workspace;
        if (data.drop === 'workspace') {
          if (to === host.workspace) return;
          run(() => moveHost(host.id, to, host.groupPath, null));
          return;
        }
        const group = data.group ? data.group : null;
        let before: string | null = null;
        if (data.drop === 'host') {
          before = target.edge === 'before' ? (data.hostId ?? null) : data.next || null;
          if (before === host.id) before = null;
        }
        run(() => moveHost(host.id, to, group, before));
      },
    });
  };

  const dragGroup = (event: React.PointerEvent, name: string) => {
    beginDrag(event, {
      label: name,
      onStart: () => setDragging(true),
      onEnd: () => setDragging(false),
      accept: (element, y) => {
        const kind = element.dataset.drop;
        if (kind === 'group' && element.dataset.head === 'true' && element.dataset.group) {
          return element.dataset.group === name ? null : edgeByHalf(element, y);
        }
        if (kind === 'workspace' && element.dataset.workspace !== workspace) return 'inside';
        return null;
      },
      onDrop: (target) => {
        const data = target.data;
        const to = (data.workspace as Workspace) ?? workspace;
        if (data.drop === 'workspace') {
          run(() => moveGroup(workspace, name, to, null));
          return;
        }
        const before = target.edge === 'before' ? (data.group ?? null) : data.nextGroup || null;
        run(() => moveGroup(workspace, name, to, before === name ? null : before));
      },
    });
  };

  // ── Menus ─────────────────────────────────────────────────────────────────

  const other: Workspace = workspace === 'private' ? 'business' : 'private';

  const hostMenu = (x: number, y: number, host: HostRecord) =>
    setMenu({
      x,
      y,
      items: [
        ...(openIds.has(host.id)
          ? [
              {
                label: t('Zum offenen Tab'),
                icon: 'monitor' as const,
                onSelect: () => props.onConnect(host),
              },
              {
                label: t('Weiteren Tab öffnen'),
                icon: 'plus' as const,
                onSelect: () => props.onConnectAnother(host),
              },
              {
                label: t('Trennen'),
                icon: 'power' as const,
                onSelect: () => props.onDisconnect(host),
              },
            ]
          : [
              {
                label: t('Verbinden'),
                icon: 'monitor' as const,
                onSelect: () => props.onConnect(host),
              },
            ]),
        { label: t('Bearbeiten'), icon: 'pencil', onSelect: () => props.onEdit(host) },
        'separator',
        ...(settings.workspaces
          ? [
              {
                label: t('Nach {workspace} verschieben', {
                  workspace: workspaceName(other, settings),
                }),
                icon: other === 'private' ? ('house' as const) : ('briefcase' as const),
                onSelect: () => run(() => moveHost(host.id, other, host.groupPath, null)),
              },
            ]
          : []),
        ...(host.groupPath
          ? [
              {
                label: t('Aus der Gruppe nehmen'),
                icon: 'up' as const,
                onSelect: () => run(() => moveHost(host.id, host.workspace, null, null)),
              },
            ]
          : []),
      ],
    });

  const groupRecord = (name: string): GroupRecord =>
    groups.find((g) => g.workspace === workspace && g.name === name) ?? {
      workspace,
      name,
      position: 0,
      username: '',
      domain: '',
      hasPassword: false,
    };

  const groupMenu = (x: number, y: number, name: string) =>
    setMenu({
      x,
      y,
      items: [
        {
          label: t('Alle verbinden'),
          icon: 'power',
          onSelect: () => props.onConnectGroup(workspace, name),
        },
        {
          label: t('Übersicht der Gruppe'),
          icon: 'grid',
          onSelect: () => props.onOverview(workspace, name),
        },
        ...(hosts.some(
          (h) => h.workspace === workspace && h.groupPath === name && openIds.has(h.id),
        )
          ? [
              {
                label: t('Alle trennen'),
                icon: 'close' as const,
                onSelect: () => props.onDisconnectGroup(workspace, name),
              },
            ]
          : []),
        'separator',
        {
          label: t('Anmeldung der Gruppe…'),
          icon: 'user',
          onSelect: () => props.onGroupLogin(groupRecord(name)),
        },
        {
          label: t('Host hier hinzufügen'),
          icon: 'plus',
          onSelect: () => props.onAdd(workspace, name),
        },
        {
          label: t('Umbenennen'),
          icon: 'pencil',
          onSelect: () => setEditing({ from: name, value: name }),
        },
        ...(settings.workspaces
          ? [
              {
                label: t('Nach {workspace} verschieben', {
                  workspace: workspaceName(other, settings),
                }),
                icon: other === 'private' ? ('house' as const) : ('briefcase' as const),
                onSelect: () => run(() => moveGroup(workspace, name, other, null)),
              },
            ]
          : []),
        'separator',
        {
          label: t('Gruppe auflösen (Hosts bleiben)'),
          icon: 'trash',
          danger: true,
          onSelect: () => run(() => deleteGroup(workspace, name)),
        },
      ],
    });

  const menuKey = (event: ReactKeyboardEvent, open: (x: number, y: number) => void) => {
    if (event.key === 'ContextMenu' || (event.shiftKey && event.key === 'F10')) {
      event.preventDefault();
      const rect = (event.currentTarget as HTMLElement).getBoundingClientRect();
      open(rect.left + 24, rect.bottom);
    }
  };

  const saveGroupName = () => {
    if (!editing) return;
    const value = editing.value.trim();
    const from = editing.from;
    setEditing(null);
    if (!value || value === from) return;
    run(() =>
      from === null ? createGroup(workspace, value) : renameGroup(workspace, from, value),
    );
  };

  // ── Rendering ─────────────────────────────────────────────────────────────

  const hostRow = (host: HostRecord, next: HostRecord | undefined, draggable: boolean) => {
    const online = onlineIds.has(host.id);
    const connecting = connectingIds.has(host.id);
    return (
      <li
        key={host.id}
        className="host-row"
        data-drop={draggable ? 'host' : undefined}
        data-host-id={host.id}
        data-next={next?.id ?? ''}
        data-workspace={host.workspace}
        data-group={host.groupPath ?? ''}
      >
        <button
          className="host"
          aria-current={activeId === host.id}
          aria-busy={connecting}
          onClick={() => props.onConnect(host)}
          onPointerDown={draggable ? (event) => dragHost(event, host) : undefined}
          onContextMenu={(event) => {
            event.preventDefault();
            hostMenu(event.clientX, event.clientY, host);
          }}
          onKeyDown={(event) => menuKey(event, (x, y) => hostMenu(x, y, host))}
          title={[
            openIds.has(host.id)
              ? t('{target} · zeigt den offenen Tab, Rechtsklick für einen weiteren', {
                  target: hostLine(host),
                })
              : t('{target} · öffnet einen neuen Tab', { target: hostLine(host) }),
            host.comment,
          ]
            .filter(Boolean)
            .join('\n')}
        >
          <span className="host-icon host-glyph">
            <Icon name="monitor" size={17} />
            <i
              className="dot"
              data-state={online ? 'online' : connecting ? 'connecting' : 'idle'}
            />
          </span>
          <span className="host-text">
            <span className="host-name">{host.name}</span>
            <span className="meta">{connecting ? t('verbindet…') : hostLine(host)}</span>
          </span>
        </button>
        <span className="host-actions">
          <button
            className="icon-button"
            onClick={() => props.onEdit(host)}
            title={t('{name} bearbeiten', { name: host.name })}
            aria-label={t('{name} bearbeiten', { name: host.name })}
          >
            <Icon name="pencil" size={15} />
          </button>
        </span>
      </li>
    );
  };

  const groupNames = sections.flatMap((section) => (section.name ? [section.name] : []));

  return (
    <aside className="sidebar" data-dragging={dragging || undefined}>
      <div className="sidebar-head">
        <h2>{t('Hosts')}</h2>
        <span className="spacer" />
        <button
          className="icon-button"
          onClick={props.onImport}
          title={t('Importieren')}
          aria-label={t('Importieren')}
        >
          <Icon name="import" />
        </button>
        <button
          className="icon-button"
          onClick={() => setEditing({ from: null, value: '' })}
          title={t('Neue Gruppe')}
          aria-label={t('Neue Gruppe')}
        >
          <Icon name="folderPlus" />
        </button>
        <button
          className="icon-button"
          onClick={() => props.onAdd(workspace, null)}
          title={t('Host hinzufügen')}
          aria-label={t('Host hinzufügen')}
        >
          <Icon name="plus" />
        </button>
      </div>

      {settings.workspaces && (
        <div className="workspace-switch" role="radiogroup" aria-label={t('Bereich')}>
          {WORKSPACES.map((id) => (
            <button
              key={id}
              type="button"
              role="radio"
              aria-checked={id === workspace}
              data-drop="workspace"
              data-workspace={id}
              onClick={() => updateSettings({ activeWorkspace: id })}
              title={t('{workspace} · Hosts hierher ziehen, um sie zu verschieben', {
                workspace: workspaceName(id, settings),
              })}
            >
              <Icon name={id === 'private' ? 'house' : 'briefcase'} size={15} />
              <span className="workspace-name">{workspaceName(id, settings)}</span>
              {counts[id] > 0 && <span className="workspace-count">{counts[id]}</span>}
            </button>
          ))}
        </div>
      )}

      {hosts.length > 0 && (
        <input
          className="search"
          type="search"
          placeholder={t('Suchen')}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          aria-label={t('Hosts durchsuchen')}
        />
      )}

      <div className="host-scroll">
        <ul className="host-list">
          <li>
            <button
              className="host"
              aria-current={activeId === 'overview'}
              onClick={() => props.onOverview(null, null)}
              title={t('Alle offenen Sitzungen als Vorschau (Strg+Umschalt+O)')}
            >
              <span className="host-icon host-glyph" aria-hidden>
                <Icon name="grid" size={17} />
              </span>
              <span className="host-name">{t('Übersicht')}</span>
              {onlineIds.size > 0 && <span className="group-count">{onlineIds.size}</span>}
            </button>
          </li>
        </ul>

        {query.trim() ? (
          <>
            <ul className="host-list">{found.map((host) => hostRow(host, undefined, false))}</ul>
            {found.length === 0 && (
              <p className="sidebar-note">{t('Kein Host passt zu „{query}".', { query })}</p>
            )}
          </>
        ) : (
          <>
            {editing?.from === null && (
              <input
                className="group-input"
                autoFocus
                placeholder={t('Name der neuen Gruppe')}
                value={editing.value}
                onChange={(e) => setEditing({ from: null, value: e.target.value })}
                onBlur={saveGroupName}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') saveGroupName();
                  if (event.key === 'Escape') setEditing(null);
                }}
                aria-label={t('Name der neuen Gruppe')}
              />
            )}

            {sections.map((section, index) => {
              if (section.name === null) {
                return (
                  <ul
                    key="ungrouped"
                    className="host-list host-dropzone"
                    data-drop="group"
                    data-workspace={workspace}
                    data-group=""
                  >
                    {section.hosts.map((host, i) => hostRow(host, section.hosts[i + 1], true))}
                  </ul>
                );
              }
              const name = section.name;
              const isCollapsed = collapsed(name);
              const nextGroup = groupNames[groupNames.indexOf(name) + 1] ?? '';
              return (
                <section
                  key={`${workspace}/${name}`}
                  className="host-group"
                  data-drop="group"
                  data-workspace={workspace}
                  data-group={name}
                  style={{ animationDelay: `${Math.min(index, 8) * 18}ms` }}
                >
                  <div
                    className="group-head"
                    data-drop="group"
                    data-head="true"
                    data-workspace={workspace}
                    data-group={name}
                    data-next-group={nextGroup}
                    onPointerDown={(event) => {
                      if ((event.target as HTMLElement).closest('input')) return;
                      dragGroup(event, name);
                    }}
                    onContextMenu={(event) => {
                      event.preventDefault();
                      groupMenu(event.clientX, event.clientY, name);
                    }}
                  >
                    {editing?.from === name ? (
                      <input
                        className="group-input"
                        autoFocus
                        value={editing.value}
                        onChange={(e) => setEditing({ from: name, value: e.target.value })}
                        onBlur={saveGroupName}
                        onKeyDown={(event) => {
                          if (event.key === 'Enter') saveGroupName();
                          if (event.key === 'Escape') setEditing(null);
                        }}
                        aria-label={t('{name} umbenennen', { name })}
                      />
                    ) : (
                      <button
                        className="group-toggle"
                        aria-expanded={!isCollapsed}
                        onClick={() => toggle(name)}
                        onDoubleClick={() => setEditing({ from: name, value: name })}
                        onKeyDown={(event) => menuKey(event, (x, y) => groupMenu(x, y, name))}
                        title={t(
                          'Klicken zum Auf-/Zuklappen, doppelklicken zum Umbenennen, ziehen zum Sortieren',
                        )}
                      >
                        <Icon name="chevron" size={12} className="group-chevron" />
                        <h3>{name}</h3>
                        {groupRecord(name).username && (
                          <span
                            className="group-login"
                            title={t('Hosts ohne eigenen Benutzer melden sich als {login} an', {
                              login: groupRecord(name).domain
                                ? `${groupRecord(name).domain}\\${groupRecord(name).username}`
                                : groupRecord(name).username,
                            })}
                          >
                            <Icon name="user" size={12} />
                          </span>
                        )}
                        <span className="group-count">{section.hosts.length}</span>
                      </button>
                    )}
                    <button
                      className="icon-button group-more"
                      onClick={(event) => {
                        const rect = event.currentTarget.getBoundingClientRect();
                        groupMenu(rect.left, rect.bottom, name);
                      }}
                      aria-label={t('Menü für {name}', { name })}
                    >
                      <Icon name="more" size={15} />
                    </button>
                  </div>
                  {!isCollapsed && (
                    <ul className="host-list">
                      {section.hosts.map((host, i) => hostRow(host, section.hosts[i + 1], true))}
                      {section.hosts.length === 0 && (
                        <li className="group-empty">{t('Hosts hierher ziehen')}</li>
                      )}
                    </ul>
                  )}
                </section>
              );
            })}
          </>
        )}

        {!query.trim() && sections.every((section) => section.hosts.length === 0) && (
          <div className="sidebar-empty">
            <Nyu size={72} mood={hosts.length === 0 ? 'uwu' : 'sleepy'} />
            <p>
              {hosts.length === 0
                ? t('Noch keine Hosts.')
                : t('Noch keine Hosts in {workspace}.', {
                    workspace: workspaceName(workspace, settings),
                  })}
            </p>
            <button className="primary" onClick={() => props.onAdd(workspace, null)}>
              {t('Host hinzufügen')}
            </button>
            <button className="quiet" onClick={props.onImport}>
              {hosts.length === 0
                ? t('Aus RDCMan (.rdg), .rdp-Dateien oder einer UwURDP-Datei importieren')
                : t('Hosts aus dem anderen Bereich hierher ziehen')}
            </button>
          </div>
        )}
      </div>

      {menu && (
        <ContextMenu
          x={menu.x}
          y={menu.y}
          items={menu.items}
          label={t('Aktionen')}
          onClose={() => setMenu(null)}
        />
      )}
    </aside>
  );
}
