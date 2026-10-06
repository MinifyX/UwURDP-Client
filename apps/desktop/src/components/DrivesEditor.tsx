import { Button, Icon, IconButton, ICONS, Toggle } from '@uwusuite/design';
import { isStoreBuild } from '../lib/build';
import { useState } from 'react';
import { t, useLanguage } from '../lib/i18n';
import {
  ALL_DRIVES,
  pickSharedFolder,
  setGroupDrives,
  type DriveRedirection,
  type GroupRecord,
  type SaveFailure,
  type SharedDrive,
} from '../lib/session';
import { Modal } from './Modal';

/** What a list of shared folders says in one line, for "like the group". */
export function drivesSummary(drives: DriveRedirection | null | undefined): string {
  if (!drives?.enabled || drives.drives.length === 0) return t('Keine Ordner freigegeben');
  return drives.drives
    .map((d) => (d.path === ALL_DRIVES ? t('Alle Laufwerke') : d.name))
    .join(', ');
}

/**
 * The list of shared folders: pick one, name it, remove it. Paths belong to
 * this computer; a folder another device added and this one lacks is
 * skipped when connecting, so it stays in the list.
 */
export function DriveList({
  drives,
  onChange,
}: {
  drives: SharedDrive[];
  onChange: (drives: SharedDrive[]) => void;
}) {
  useLanguage();
  const [error, setError] = useState<string | null>(null);
  const hasAll = drives.some((d) => d.path === ALL_DRIVES);

  const add = async () => {
    setError(null);
    try {
      const picked = await pickSharedFolder();
      if (picked && !drives.some((d) => d.path === picked.path)) onChange([...drives, picked]);
    } catch (raw) {
      setError(String(raw));
    }
  };

  return (
    <div className="drive-list">
      {drives.length === 0 && (
        <p className="field-hint">
          {t('Noch kein Ordner. Füge einen Ordner oder ein Laufwerk hinzu.')}
        </p>
      )}
      {drives.map((drive, index) => (
        <div className="drive-row" key={drive.path}>
          <Icon icon={drive.path === ALL_DRIVES ? ICONS.drive : ICONS.folder} />
          {drive.path === ALL_DRIVES ? (
            <span className="drive-all">
              <b>{t('Alle Laufwerke')}</b>
              <small>
                {t('Jedes feste Laufwerk dieses Computers, mit seinem Buchstaben (nur Windows).')}
              </small>
            </span>
          ) : (
            <>
              <input
                className="drive-name"
                value={drive.name}
                aria-label={t('Name auf dem Server')}
                title={t('Name auf dem Server')}
                maxLength={32}
                spellCheck={false}
                onChange={(e) =>
                  onChange(drives.map((d, i) => (i === index ? { ...d, name: e.target.value } : d)))
                }
              />
              <code className="drive-path" title={drive.path}>
                {drive.path}
              </code>
            </>
          )}
          <IconButton
            type="button"
            size="sm"
            icon={ICONS.delete}
            label={t('Entfernen')}
            onClick={() => onChange(drives.filter((_, i) => i !== index))}
          />
        </div>
      ))}
      <div className="drive-actions">
        <Button type="button" onClick={() => void add()} icon={ICONS.add}>
          {t('Ordner hinzufügen…')}
        </Button>
        {!hasAll && !isStoreBuild() && (
          <Button
            type="button"
            variant="ghost"
            onClick={() => onChange([...drives, { name: ALL_DRIVES, path: ALL_DRIVES }])}
            icon={ICONS.drive}
          >
            {t('Alle Laufwerke')}
          </Button>
        )}
      </div>
      {error && <p className="form-error">{error}</p>}
      <em className="field-hint">
        {t(
          'Auf dem Server unter „Dieser PC“ und als \\\\tsclient\\Name. Die Liste wird mit synchronisiert; ein Ordner, den es auf einem anderen Gerät nicht gibt, wird dort beim Verbinden übersprungen.',
        )}
      </em>
    </div>
  );
}

/** A switch with the list under it, for a group or a host's own setting. */
export function DrivesSwitch({
  value,
  onChange,
}: {
  value: DriveRedirection;
  onChange: (value: DriveRedirection) => void;
}) {
  useLanguage();
  return (
    <>
      <Toggle
        label={t('Lokale Laufwerke/Ordner freigeben')}
        description={t('Wie in mstsc: Der Server kann die Ordner lesen und beschreiben.')}
        checked={value.enabled}
        onChange={(enabled) => onChange({ ...value, enabled })}
      />
      {value.enabled && (
        <div className="gateway-block">
          <DriveList drives={value.drives} onChange={(drives) => onChange({ ...value, drives })} />
        </div>
      )}
    </>
  );
}

/** Right-click a group → "Lokale Ordner…": what every host in it shares unless it says otherwise. */
export function GroupDrivesDialog({
  group,
  onSaved,
  onCancel,
}: {
  group: GroupRecord;
  onSaved: () => void;
  onCancel: () => void;
}) {
  useLanguage();
  const [value, setValue] = useState<DriveRedirection>(
    group.drives ?? { enabled: false, drives: [] },
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      await setGroupDrives(group.workspace, group.name, value.enabled ? value : null);
      onSaved();
    } catch (raw) {
      const failure = raw as SaveFailure;
      setError(failure?.kind === 'error' ? failure.message : String(raw));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title={t('Lokale Ordner für {group}', { group: group.name })}
      onCancel={onCancel}
      footer={
        <>
          <span className="spacer" />
          <Button data-secondary onClick={onCancel} disabled={busy}>
            {t('Abbrechen')}
          </Button>
          <Button variant="primary" onClick={() => void save()} busy={busy}>
            {t('Speichern')}
          </Button>
        </>
      }
    >
      <div className="form">
        <p className="dialog-lead">
          {t(
            'Jeder Host in dieser Gruppe, der nichts Eigenes eingestellt hat, gibt diese Ordner frei.',
          )}
        </p>
        <DrivesSwitch value={value} onChange={setValue} />
        {error && <p className="form-error">{error}</p>}
      </div>
    </Modal>
  );
}
