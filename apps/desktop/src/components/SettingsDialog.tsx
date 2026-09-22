import { useEffect, useState, type ReactNode } from 'react';
import pkg from '../../package.json';
import { N_, t, useLanguage } from '../lib/i18n';
import {
  checkForUpdates,
  listHosts,
  lockVault,
  openProjectPage,
  setVaultRemembered,
  vaultState,
  type HostRecord,
  type ProjectPage,
  type UpdateInfo,
  type VaultState,
} from '../lib/session';
import { updateSettings, useSettings, workspaceName, type StartupSetting } from '../lib/settings';
import { systemName } from '../lib/platform';
import { hostLine } from '../lib/tabs';
import { ExportDialog } from './ExportDialog';
import { SyncSettings } from './SyncSettings';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { Nyu } from './nyu/Nyu';
import { VaultDialog } from './VaultDialog';

export type SettingsSection =
  'appearance' | 'sessions' | 'vault' | 'sync' | 'data' | 'updates' | 'about';

const SECTIONS: { id: SettingsSection; label: string }[] = [
  { id: 'appearance', label: N_('Darstellung') },
  { id: 'sessions', label: N_('Sitzungen') },
  { id: 'vault', label: N_('Tresor') },
  { id: 'sync', label: N_('Sync') },
  { id: 'data', label: N_('Import & Export') },
  { id: 'updates', label: N_('Updates') },
  { id: 'about', label: N_('Über UwURDP') },
];

type Props = {
  initial?: SettingsSection;
  onClose: () => void;
  /** A downloaded update, if one is waiting. */
  update: UpdateInfo | null;
  onUpdateFound: (update: UpdateInfo) => void;
  onInstallUpdate: () => void;
  onImport: () => void;
  /** Hosts changed from here: the host list reloads. */
  onChanged: () => void;
};

/** One setting: a label, an optional explanation and its control. */
function Row({
  label,
  description,
  children,
}: {
  label: string;
  description?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="setting-row">
      <div className="setting-text">
        <p className="setting-label">{label}</p>
        {description && <p className="setting-description">{description}</p>}
      </div>
      <div className="setting-control">{children}</div>
    </div>
  );
}

function Segmented<T extends string | number>({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (value: T) => void;
}) {
  return (
    <div className="segmented" role="radiogroup" aria-label={label}>
      {options.map((option) => (
        <button
          key={String(option.value)}
          type="button"
          role="radio"
          aria-checked={option.value === value}
          onClick={() => onChange(option.value)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

function Toggle({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <button
      type="button"
      role="switch"
      className="toggle"
      aria-checked={checked}
      aria-label={label}
      onClick={() => onChange(!checked)}
    >
      <span className="toggle-thumb" />
    </button>
  );
}

function Appearance() {
  const settings = useSettings();
  return (
    <>
      <Row
        label="Sprache · Language"
        description={`„System“ folgt der Sprache von ${systemName()}. · “System” follows ${systemName()}.`}
      >
        <Segmented
          label="Sprache · Language"
          value={settings.language}
          onChange={(language) => updateSettings({ language })}
          options={[
            { value: 'system', label: 'System' },
            { value: 'de', label: 'Deutsch' },
            { value: 'en', label: 'English' },
          ]}
        />
      </Row>
      <Row label={t('Farbschema')}>
        <Segmented
          label={t('Farbschema')}
          value={settings.theme}
          onChange={(theme) => updateSettings({ theme })}
          options={[
            { value: 'system', label: t('System') },
            { value: 'light', label: t('Hell') },
            { value: 'dark', label: t('Dunkel') },
          ]}
        />
      </Row>
      <Row
        label={t('Animationen')}
        description={t('„System“ folgt der Einstellung von {system}.', { system: systemName() })}
      >
        <Segmented
          label={t('Animationen')}
          value={settings.motion}
          onChange={(motion) => updateSettings({ motion })}
          options={[
            { value: 'system', label: t('System') },
            { value: 'on', label: t('An') },
            { value: 'off', label: t('Aus') },
          ]}
        />
      </Row>
      <Row
        label={t('Privat und Business')}
        description={t(
          'Zwei Bereiche in der Hostliste, wie bei UwUMail. Hosts und Gruppen ziehst du einfach in den anderen Bereich.',
        )}
      >
        <Toggle
          label={t('Privat und Business')}
          checked={settings.workspaces}
          onChange={(workspaces) => updateSettings({ workspaces })}
        />
      </Row>
      {settings.workspaces && (
        <Row
          label={t('Namen der Bereiche')}
          description={t('Leer lassen für „Privat“ und „Business“.')}
        >
          <div className="workspace-names">
            {(['private', 'business'] as const).map((id) => (
              <input
                key={id}
                className="search"
                value={settings.workspaceNames[id]}
                placeholder={id === 'private' ? t('Privat') : t('Business')}
                maxLength={24}
                aria-label={t('Name für {name}', { name: workspaceName(id, settings) })}
                onChange={(e) =>
                  updateSettings({
                    workspaceNames: { ...settings.workspaceNames, [id]: e.target.value },
                  })
                }
              />
            ))}
          </div>
        </Row>
      )}
    </>
  );
}

function Sessions() {
  const settings = useSettings();
  return (
    <>
      <StartupRow />
      <Row
        label={t('Automatisch neu verbinden')}
        description={t(
          'Bricht eine Sitzung ab, ohne dass du oder der Server sie beendet habt – etwa weil das WLAN kurz weg war –, versucht UwURDP es einmal von selbst.',
        )}
      >
        <Toggle
          label={t('Automatisch neu verbinden')}
          checked={settings.autoReconnect}
          onChange={(autoReconnect) => updateSettings({ autoReconnect })}
        />
      </Row>
      <Row
        label={t('Vor dem Schließen nachfragen')}
        description={t('Wenn noch Sitzungen offen sind.')}
      >
        <Toggle
          label={t('Vor dem Schließen nachfragen')}
          checked={settings.confirmCloseWithSessions}
          onChange={(confirmCloseWithSessions) => updateSettings({ confirmCloseWithSessions })}
        />
      </Row>
      <div className="shortcuts">
        <p className="setting-label">{t('Tastenkürzel')}</p>
        <p className="setting-description">
          {t(
            'Im Remotedesktop gehören alle Tasten dem Server – bis auf die mit Strg+Alt, die auch mstsc für sich behält.',
          )}
        </p>
        <dl>
          <dt>{t('Strg+Alt+Ende')}</dt>
          <dd>{t('Strg+Alt+Entf an den Server')}</dd>
          <dt>{t('Strg+Alt+Pause')}</dt>
          <dd>{t('Vollbild an · aus')}</dd>
          <dt>{t('Strg+Alt+Pos1')}</dt>
          <dd>{t('Tastatur zurück an UwURDP')}</dd>
          <dt>{t('Strg+Alt+Bild↑ · Bild↓')}</dt>
          <dd>{t('Vorheriger · nächster Tab')}</dd>
          <dt>{t('Strg+Umschalt+O')}</dt>
          <dd>{t('Übersicht')}</dd>
          <dt>{t('Strg+Umschalt+D')}</dt>
          <dd>{t('Tab duplizieren (neue Verbindung zum selben Host)')}</dd>
          <dt>{t('Strg+Umschalt+W')}</dt>
          <dd>{t('Tab schließen')}</dd>
          <dt>{t('Strg+Umschalt+1 … 9')}</dt>
          <dd>{t('Zu Tab 1 … 9')}</dd>
          <dt>{t('Strg+,')}</dt>
          <dd>{t('Einstellungen')}</dd>
        </dl>
      </div>
    </>
  );
}

/** What opens when UwURDP starts: nothing (the default), the overview, or chosen hosts. */
function StartupRow() {
  const settings = useSettings();
  const [hosts, setHosts] = useState<HostRecord[] | null>(null);
  useEffect(() => {
    if (settings.startup !== 'hosts' || hosts) return;
    void listHosts()
      .then(setHosts)
      .catch(() => setHosts([]));
  }, [settings.startup, hosts]);
  const chosen = new Set(settings.startupHosts);
  const toggle = (id: string, on: boolean) =>
    updateSettings({
      startupHosts: on
        ? [...settings.startupHosts.filter((other) => other !== id), id]
        : settings.startupHosts.filter((other) => other !== id),
    });
  return (
    <>
      <Row
        label={t('Beim Start öffnen')}
        description={t(
          'Normalerweise öffnet UwURDP keine Verbindung von selbst. Hier kannst du die Übersicht oder bestimmte Hosts vorgeben.',
        )}
      >
        <Segmented<StartupSetting>
          label={t('Beim Start öffnen')}
          value={settings.startup}
          onChange={(startup) => updateSettings({ startup })}
          options={[
            { value: 'nothing', label: t('Nichts') },
            { value: 'overview', label: t('Übersicht') },
            { value: 'hosts', label: t('Hosts') },
          ]}
        />
      </Row>
      {settings.startup === 'hosts' && (
        <div className="startup-hosts" role="group" aria-label={t('Hosts beim Start')}>
          {hosts === null ? (
            <p className="setting-description">{t('Hosts werden geladen…')}</p>
          ) : hosts.length === 0 ? (
            <p className="setting-description">{t('Noch keine Hosts angelegt.')}</p>
          ) : (
            [...hosts]
              .sort((a, b) => a.name.localeCompare(b.name, 'de'))
              .map((host) => (
                <label key={host.id} className="check inline">
                  <input
                    type="checkbox"
                    checked={chosen.has(host.id)}
                    onChange={(event) => toggle(host.id, event.target.checked)}
                  />
                  <span>
                    {host.name}
                    <small>{hostLine(host)}</small>
                  </span>
                </label>
              ))
          )}
          {hosts !== null && hosts.length > 0 && chosen.size === 0 && (
            <p className="setting-description">
              {t('Keiner gewählt – dann öffnet beim Start nichts.')}
            </p>
          )}
        </div>
      )}
    </>
  );
}

function Vault({ onChanged }: { onChanged: () => void }) {
  useLanguage();
  const [state, setState] = useState<VaultState | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dialog, setDialog] = useState(false);

  const load = () => {
    void vaultState()
      .then(setState)
      .catch((e) => setError(String(e)));
  };

  useEffect(load, []);

  const guard = async (action: () => Promise<unknown>) => {
    setError(null);
    try {
      await action();
    } catch (e) {
      const failure = e as { kind?: string; message?: string };
      if (failure?.kind === 'vault-locked') setDialog(true);
      else setError(failure?.message ?? String(e));
    }
    load();
    onChanged();
  };

  const status = state?.status;
  const text: Record<VaultState['status'], string> = {
    absent: t('Noch kein Tresor. Er entsteht, sobald du ein Passwort speicherst.'),
    locked: t(
      'Gesperrt. UwURDP fragt nach dem Master-Passwort, sobald etwas daraus gebraucht wird.',
    ),
    unlocked: t('Entsperrt. Gespeicherte Passwörter können benutzt werden.'),
  };

  return (
    <>
      <Row
        label={t('Status')}
        description={t(
          'Passwörter liegen verschlüsselt im Tresor, mit Argon2id und XChaCha20-Poly1305. Das Master-Passwort verlässt dieses Gerät nie.',
        )}
      >
        <span className="vault-status" data-status={status ?? 'loading'}>
          {status ? text[status] : error ? error : t('Wird geprüft …')}
        </span>
        {status !== 'unlocked' && (
          <button className="primary" onClick={() => setDialog(true)}>
            {status === 'absent' ? t('Anlegen') : t('Entsperren')}
          </button>
        )}
      </Row>
      {status !== 'absent' && (
        <Row
          label={t('Auf diesem Gerät merken')}
          description={t(
            '{system} öffnet den Tresor beim Start für dein Benutzerkonto, ohne Master-Passwort. Andere Konten und andere Rechner brauchen es weiter.',
            { system: systemName() },
          )}
        >
          <Toggle
            label={t('Auf diesem Gerät merken')}
            checked={Boolean(state?.remembered)}
            onChange={(remember) =>
              void guard(async () => {
                if (remember && status !== 'unlocked') {
                  setDialog(true);
                  return;
                }
                await setVaultRemembered(remember);
              })
            }
          />
        </Row>
      )}
      {status === 'unlocked' && (
        <Row
          label={t('Tresor sperren')}
          description={
            state?.remembered
              ? t('Bis zum nächsten Start. Offene Verbindungen bleiben bestehen.')
              : t(
                  'Offene Verbindungen bleiben bestehen; neue fragen wieder nach dem Master-Passwort.',
                )
          }
        >
          <button onClick={() => void guard(lockVault)}>{t('Jetzt sperren')}</button>
        </Row>
      )}
      {error && (
        <p className="setting-result" data-tone="error">
          {error}
        </p>
      )}
      {dialog && (
        <VaultDialog
          onDone={() => {
            setDialog(false);
            load();
          }}
          onCancel={() => setDialog(false)}
        />
      )}
    </>
  );
}

function Data({ onImport }: { onImport: () => void }) {
  useLanguage();
  const [exporting, setExporting] = useState(false);
  return (
    <>
      <Row
        label={t('Exportieren')}
        description={t(
          'Alle Hosts mit Bereichen, Gruppen, Anmeldungen und Zertifikaten in eine .uwurdp-Datei – auf Wunsch mit Passwörtern, dann mit eigenem Passwort verschlüsselt.',
        )}
      >
        <button onClick={() => setExporting(true)}>
          <Icon name="export" size={15} />
          {t('Exportieren…')}
        </button>
      </Row>
      <Row
        label={t('Importieren')}
        description={t(
          'Aus RDCMan (.rdg), .rdp-Dateien oder einer .uwurdp-Datei. Schon vorhandene Hosts werden übersprungen.',
        )}
      >
        <button onClick={onImport}>
          <Icon name="import" size={15} />
          {t('Importieren…')}
        </button>
      </Row>
      {exporting && <ExportDialog onClose={() => setExporting(false)} />}
    </>
  );
}

function Updates({
  update,
  onUpdateFound,
  onInstallUpdate,
}: Pick<Props, 'update' | 'onUpdateFound' | 'onInstallUpdate'>) {
  const settings = useSettings();
  const [checking, setChecking] = useState(false);
  const [result, setResult] = useState<{ tone: 'info' | 'error'; text: string } | null>(null);

  return (
    <>
      <Row
        label={t('Update-Kanal')}
        description={
          settings.updateChannel === 'beta'
            ? t('Beta bekommt neue Versionen früher. Es kann mal etwas wackeln.')
            : t('Stabil bekommt nur fertige Versionen.')
        }
      >
        <Segmented
          label={t('Update-Kanal')}
          value={settings.updateChannel}
          onChange={(updateChannel) => {
            setResult(null);
            updateSettings({ updateChannel });
          }}
          options={[
            { value: 'stable', label: t('Stabil') },
            { value: 'beta', label: t('Beta') },
          ]}
        />
      </Row>
      <Row
        label={t('Version {version}', { version: pkg.version })}
        description={t(
          'UwURDP lädt neue Versionen still herunter und installiert sie beim nächsten Start. Jedes Update ist signiert und wird vor dem Start geprüft.',
        )}
      >
        {update ? (
          <button className="primary" onClick={onInstallUpdate}>
            {t('{version} installieren', { version: update.version })}
          </button>
        ) : (
          <button
            disabled={checking}
            onClick={async () => {
              setChecking(true);
              setResult(null);
              try {
                const found = await checkForUpdates();
                if (found) onUpdateFound(found);
                else setResult({ tone: 'info', text: t('UwURDP ist auf dem neuesten Stand. ✧') });
              } catch (e) {
                setResult({
                  tone: 'error',
                  text: t('Suche fehlgeschlagen: {error}', { error: String(e) }),
                });
              } finally {
                setChecking(false);
              }
            }}
          >
            {checking ? t('Sucht …') : t('Nach Updates suchen')}
          </button>
        )}
      </Row>
      {result && (
        <p className="setting-result" data-tone={result.tone} role="status">
          {result.text}
        </p>
      )}
    </>
  );
}

function About() {
  useLanguage();
  const open = (page: ProjectPage) => void openProjectPage(page).catch(() => undefined);
  return (
    <div className="about">
      <Nyu size={88} mood="happy" title="Nyu" />
      <p className="about-name">
        <span>UwU</span>RDP
      </p>
      <p className="about-version">{t('Version {version}', { version: pkg.version })}</p>
      <p className="about-text">
        {t(
          'Freie Software unter der GNU GPL v3.0. Nutzen, ändern, weitergeben – nur geänderte Versionen müssen offen bleiben. Kein Tracking, kein Konto.',
        )}
      </p>
      <p className="about-text">{t('Remotedesktop mit IronRDP von Devolutions, in Rust.')}</p>
      <div className="about-actions">
        <button onClick={() => open('source')}>{t('Quellcode auf GitHub')}</button>
        <button onClick={() => open('releases')}>{t('Versionen')}</button>
        <button onClick={() => open('license')}>{t('Lizenz')}</button>
      </div>
    </div>
  );
}

export function SettingsDialog({
  initial = 'appearance',
  onClose,
  update,
  onUpdateFound,
  onInstallUpdate,
  onImport,
  onChanged,
}: Props) {
  useLanguage();
  const [section, setSection] = useState<SettingsSection>(initial);
  return (
    <Modal title={t('Einstellungen')} size="wide" onCancel={onClose}>
      <div className="settings">
        <nav className="settings-nav" aria-label={t('Bereiche')}>
          {SECTIONS.map(({ id, label }) => (
            <button
              key={id}
              type="button"
              aria-current={section === id ? 'page' : undefined}
              onClick={() => setSection(id)}
            >
              {t(label)}
            </button>
          ))}
        </nav>
        <div className="settings-content">
          {section === 'appearance' && <Appearance />}
          {section === 'sessions' && <Sessions />}
          {section === 'vault' && <Vault onChanged={onChanged} />}
          {section === 'sync' && <SyncSettings />}
          {section === 'data' && <Data onImport={onImport} />}
          {section === 'updates' && (
            <Updates
              update={update}
              onUpdateFound={onUpdateFound}
              onInstallUpdate={onInstallUpdate}
            />
          )}
          {section === 'about' && <About />}
        </div>
      </div>
      <button className="settings-close icon-button" onClick={onClose} aria-label={t('Schließen')}>
        ×
      </button>
    </Modal>
  );
}
