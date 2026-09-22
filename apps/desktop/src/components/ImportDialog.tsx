import { useEffect, useState } from 'react';
import {
  asBackupFailure,
  importExportFile,
  pickExportFile,
  readExportFile,
  type BackupSummary,
  type PickedExport,
} from '../lib/backup';
import { t, useLanguage } from '../lib/i18n';
import { getSettings } from '../lib/settings';
import {
  pickImportFiles,
  rdcmanFiles,
  runImport,
  scanRdcmanFile,
  type ImportReport,
  type ImportSummary,
  type RdcManFile,
} from '../lib/session';
import { Icon } from './Icon';
import { useCloseGuard } from './CloseGuard';
import { Modal } from './Modal';
import { NyuScene } from './nyu/scenes';
import { VaultDialog } from './VaultDialog';

type Props = {
  onClose: () => void;
  /** Called after a successful import, so the host list can refresh. */
  onImported: () => void;
};

type Step =
  | { kind: 'loading' }
  | { kind: 'pick'; rdcman: RdcManFile[] }
  | { kind: 'preview'; summary: ImportSummary }
  | { kind: 'file-password'; file: PickedExport; wrong: boolean }
  | { kind: 'file-preview'; file: PickedExport; summary: BackupSummary; password: string | null }
  | { kind: 'done'; report: ImportReport };

/**
 * Bringing a setup across: RDCMan's `.rdg` files (the ones RDCMan had open are
 * listed right away), `.rdp` files saved from mstsc, or an UwURDP export. Pick
 * a source, see what it holds in counts, and write it. Passwords need the
 * vault; the vault dialog comes in between and the import goes on right after.
 * Previews and results show counts only, never a host or a secret.
 */
export function ImportDialog({ onClose, onImported }: Props) {
  useLanguage();
  const [step, setStep] = useState<Step>({ kind: 'loading' });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [vaultFor, setVaultFor] = useState<(() => Promise<void>) | null>(null);
  const [password, setPassword] = useState('');
  const closeGuard = useCloseGuard(
    step.kind === 'preview' || step.kind === 'file-password' || step.kind === 'file-preview',
    onClose,
    t('Der Import wird dann nicht ausgeführt.'),
  );

  useEffect(() => {
    void guard(async () => {
      const rdcman = await rdcmanFiles().catch(() => [] as RdcManFile[]);
      setStep({ kind: 'pick', rdcman });
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** Run an action with the buttons disabled and the error line cleared. */
  async function guard(action: () => Promise<void>) {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (e) {
      const failure = asBackupFailure(e);
      setError(failure.kind === 'error' ? failure.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  /**
   * Run `write`. When it has a password to seal and the vault is locked, it
   * fails with `vault-locked`, having written nothing: open the vault, then
   * run it again. Hosts already imported bring no password, so importing the
   * same file twice never asks.
   */
  async function withVault(write: () => Promise<void>) {
    try {
      await write();
    } catch (e) {
      if (asBackupFailure(e).kind !== 'vault-locked') throw e;
      setVaultFor(() => write);
    }
  }

  async function pickFiles(kind: 'rdg' | 'rdp') {
    const summary = await pickImportFiles(kind);
    if (summary) setStep({ kind: 'preview', summary });
  }

  async function writeImport() {
    await withVault(async () => {
      // Into the workspace the host list shows, like a host added by hand.
      const { workspaces, activeWorkspace } = getSettings();
      const report = await runImport(workspaces ? activeWorkspace : 'private');
      onImported();
      setStep({ kind: 'done', report });
    });
  }

  async function pickFile() {
    const file = await pickExportFile();
    if (!file) return;
    if (file.sealed || !file.summary) setStep({ kind: 'file-password', file, wrong: false });
    else setStep({ kind: 'file-preview', file, summary: file.summary, password: null });
  }

  async function unlockFile(file: PickedExport) {
    const entered = password;
    try {
      const summary = await readExportFile(file.token, entered);
      setStep({ kind: 'file-preview', file, summary, password: entered });
    } catch (e) {
      if (asBackupFailure(e).kind === 'password-wrong') {
        setStep({ kind: 'file-password', file, wrong: true });
        setPassword('');
      } else throw e;
    }
  }

  async function importFile(file: PickedExport, filePassword: string | null) {
    await withVault(async () => {
      const report = await importExportFile(file.token, filePassword);
      setPassword('');
      onImported();
      setStep({ kind: 'done', report });
    });
  }

  const title = step.kind === 'done' ? t('Import abgeschlossen ✧') : t('Importieren');

  return (
    <>
      <Modal title={title} onCancel={closeGuard.request} footer={footer()}>
        {error && (
          <p className="field-error" role="alert">
            {error}
          </p>
        )}
        {body()}
      </Modal>
      {closeGuard.dialog}
      {vaultFor && (
        <VaultDialog
          reason={t('Die importierten Passwörter landen verschlüsselt im Tresor.')}
          onDone={() => {
            const write = vaultFor;
            setVaultFor(null);
            void guard(write);
          }}
          onCancel={() => setVaultFor(null)}
        />
      )}
    </>
  );

  function body() {
    switch (step.kind) {
      case 'loading':
        return <p className="import-note">{t('Wird gesucht…')}</p>;

      case 'pick':
        return (
          <div className="import-sources">
            <p className="import-note">{t('Woraus möchtest du importieren?')}</p>
            {step.rdcman.length > 0 && (
              <>
                <p className="setting-label">{t('Zuletzt im RDCMan geöffnet')}</p>
                {step.rdcman.map((file) => (
                  <button
                    key={file.token}
                    className="import-source"
                    disabled={busy}
                    title={file.folder}
                    onClick={() =>
                      void guard(async () =>
                        setStep({ kind: 'preview', summary: await scanRdcmanFile(file.token) }),
                      )
                    }
                  >
                    <Icon name="file" size={16} /> {file.name}
                  </button>
                ))}
              </>
            )}
            <button
              className="import-source"
              disabled={busy}
              onClick={() => void guard(() => pickFiles('rdg'))}
              title={t('Remote Desktop Connection Manager von Sysinternals, 2.2 bis 2.93')}
            >
              <Icon name="folder" size={16} /> {t('RDCMan-Datei (.rdg)…')}
            </button>
            <button
              className="import-source"
              disabled={busy}
              onClick={() => void guard(() => pickFiles('rdp'))}
              title={t('Gespeicherte Verbindungen aus mstsc, auch mehrere auf einmal')}
            >
              <Icon name="monitor" size={16} /> {t('RDP-Dateien (.rdp)…')}
            </button>
            <button className="import-source" disabled={busy} onClick={() => void guard(pickFile)}>
              <Icon name="file" size={16} /> {t('UwURDP-Export (.uwurdp)…')}
            </button>
            <p className="import-note">
              {t(
                'Gespeicherte Passwörter aus RDCMan und .rdp-Dateien sind mit deinem Windows-Konto verschlüsselt – UwURDP kann sie nur auf dem Rechner und unter dem Konto lesen, wo sie gespeichert wurden.',
              )}
            </p>
          </div>
        );

      case 'preview':
        return (
          <Preview
            source={step.summary.label}
            counts={[
              [t('Hosts'), step.summary.hosts],
              [t('Gruppen'), step.summary.groups],
              [t('Anmeldungen'), step.summary.logins],
              [t('Passwörter'), step.summary.passwords],
              [t('Gateways'), step.summary.gateways],
            ]}
            secrets={step.summary.needsVault}
            skipped={step.summary.skipped}
          />
        );

      case 'file-password': {
        const [beforeFile, afterFile] = t(
          '{file} ist mit einem Passwort geschützt, weil Passwörter darin stecken.',
        ).split('{file}');
        return (
          <form
            className="form"
            onSubmit={(event) => {
              event.preventDefault();
              void guard(() => unlockFile(step.file));
            }}
          >
            <p className="dialog-lead">
              {beforeFile}
              <code>{step.file.fileName}</code>
              {afterFile}
            </p>
            <label className="field">
              <span>{t('Passwort der Export-Datei')}</span>
              <input
                type="password"
                data-autofocus
                value={password}
                autoComplete="off"
                aria-invalid={step.wrong}
                onChange={(e) => setPassword(e.target.value)}
              />
              {step.wrong && <em className="field-error">{t('Das Passwort passt nicht.')}</em>}
            </label>
            <button type="submit" hidden />
          </form>
        );
      }

      case 'file-preview':
        return (
          <Preview
            source={step.file.fileName}
            counts={[
              [t('Hosts'), step.summary.hosts],
              [t('Gruppen'), step.summary.groups],
              [t('Passwörter'), step.summary.passwords],
              [t('Bekannte Zertifikate'), step.summary.knownHosts],
            ]}
            secrets={step.summary.passwords > 0}
            skipped={[]}
          />
        );

      case 'done':
        return <Report report={step.report} />;
    }
  }

  function footer() {
    switch (step.kind) {
      case 'preview': {
        const nothing = step.summary.hosts === 0 && step.summary.groups === 0;
        return (
          <>
            <span className="spacer" />
            <button data-secondary onClick={closeGuard.request}>
              {t('Abbrechen')}
            </button>
            <button
              className="primary"
              disabled={busy || nothing}
              onClick={() => void guard(writeImport)}
            >
              {busy ? t('Importiere…') : t('Importieren')}
            </button>
          </>
        );
      }
      case 'file-password':
        return (
          <>
            <span className="spacer" />
            <button data-secondary onClick={closeGuard.request}>
              {t('Abbrechen')}
            </button>
            <button
              className="primary"
              disabled={busy || !password}
              onClick={() => void guard(() => unlockFile(step.file))}
            >
              {t('Öffnen')}
            </button>
          </>
        );
      case 'file-preview': {
        const nothing = step.summary.hosts === 0 && step.summary.groups === 0;
        return (
          <>
            <span className="spacer" />
            <button data-secondary onClick={closeGuard.request}>
              {t('Abbrechen')}
            </button>
            <button
              className="primary"
              disabled={busy || nothing}
              onClick={() => void guard(() => importFile(step.file, step.password))}
            >
              {busy ? t('Importiere…') : t('Importieren')}
            </button>
          </>
        );
      }
      case 'done':
        return (
          <>
            <span className="spacer" />
            <button className="primary" onClick={onClose}>
              {t('Fertig')}
            </button>
          </>
        );
      default:
        return (
          <>
            <span className="spacer" />
            <button data-secondary onClick={closeGuard.request}>
              {t('Abbrechen')}
            </button>
          </>
        );
    }
  }
}

function Preview({
  source,
  counts,
  secrets,
  skipped,
}: {
  source: string;
  counts: [string, number][];
  secrets: boolean;
  skipped: string[];
}) {
  useLanguage();
  return (
    <div className="import-preview">
      <p className="import-note">
        {t(
          'Das findet UwURDP in {source}. Schon vorhandene Hosts (gleiche Adresse, gleicher Port) werden übersprungen, nichts wird überschrieben.',
          { source },
        )}
      </p>
      <ul className="import-counts">
        {counts
          .filter(([, count]) => count > 0)
          .map(([label, count]) => (
            <li key={label}>
              <b>{count}</b>
              <span>{label}</span>
            </li>
          ))}
      </ul>
      {secrets && (
        <p className="import-note">{t('Die Passwörter landen verschlüsselt im Tresor.')}</p>
      )}
      <Skipped items={skipped} />
    </div>
  );
}

function Report({ report }: { report: ImportReport }) {
  useLanguage();
  const rows: [string, number][] = [
    [t('Hosts hinzugefügt'), report.hostsAdded],
    [t('Hosts übersprungen (schon da)'), report.hostsSkipped],
    [t('Gruppen'), report.groupsAdded],
    [t('Anmeldungen'), report.loginsAdded],
    [t('Passwörter'), report.passwordsAdded],
  ];
  return (
    <div className="import-preview">
      <NyuScene name="done" className="dialog-scene" />
      <ul className="import-counts">
        {rows.map(([label, count]) => (
          <li key={label}>
            <b>{count}</b>
            <span>{label}</span>
          </li>
        ))}
      </ul>
      <Skipped items={report.skipped} />
    </div>
  );
}

function Skipped({ items }: { items: string[] }) {
  useLanguage();
  if (items.length === 0) return null;
  return (
    <details className="import-skipped">
      <summary>
        {items.length === 1
          ? t('1 Eintrag übersprungen')
          : t('{n} Einträge übersprungen', { n: items.length })}
      </summary>
      <ul>
        {items.map((item, index) => (
          <li key={index}>{item}</li>
        ))}
      </ul>
    </details>
  );
}
