import { useState } from 'react';
import { asBackupFailure, exportHosts, type BackupSummary } from '../lib/backup';
import { t, useLanguage } from '../lib/i18n';
import { Modal } from './Modal';
import { useCloseGuard } from './CloseGuard';
import { NyuScene } from './nyu/scenes';
import { VaultDialog } from './VaultDialog';

/**
 * Everything into one `.uwurdp` file: hosts, groups, trusted certificates, and
 * — if wanted — stored passwords, then sealed with a password of the
 * file's own. Read back with Importieren → UwURDP-Export.
 */
export function ExportDialog({ onClose }: { onClose: () => void }) {
  useLanguage();
  const [secrets, setSecrets] = useState(true);
  const [password, setPassword] = useState('');
  const [repeat, setRepeat] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [vault, setVault] = useState(false);
  const [done, setDone] = useState<{ fileName: string; summary: BackupSummary } | null>(null);
  const guard = useCloseGuard(
    !done && password.length > 0,
    onClose,
    t('Das eingegebene Passwort für die Datei geht dabei verloren.'),
  );

  const mismatch = secrets && repeat.length > 0 && password !== repeat;
  const ready = !busy && (!secrets || (password.length >= 8 && password === repeat));

  const run = async () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    try {
      const result = await exportHosts(secrets, secrets ? password : null);
      if (result) {
        setPassword('');
        setRepeat('');
        setDone(result);
      }
    } catch (e) {
      const failure = asBackupFailure(e);
      if (failure.kind === 'vault-locked') setVault(true);
      else
        setError(
          failure.kind === 'error' ? failure.message : t('Fehler ({kind})', { kind: failure.kind }),
        );
    } finally {
      setBusy(false);
    }
  };

  if (vault) {
    return (
      <VaultDialog
        reason={t('Um Passwörter mitzunehmen, muss der Tresor offen sein.')}
        onDone={() => {
          setVault(false);
          void run();
        }}
        onCancel={() => setVault(false)}
      />
    );
  }

  if (done) {
    const { summary } = done;
    return (
      <Modal
        title={t('Export gespeichert ✧')}
        onCancel={onClose}
        footer={
          <>
            <span className="spacer" />
            <button className="primary" onClick={onClose}>
              {t('Fertig')}
            </button>
          </>
        }
      >
        <NyuScene name="done" className="dialog-scene" />
        <p className="dialog-lead">
          <code>{done.fileName}</code>
        </p>
        <ul className="import-counts">
          {(
            [
              [t('Hosts'), summary.hosts],
              [t('Gruppen'), summary.groups],
              [t('Passwörter'), summary.passwords],
              [t('Zertifikate'), summary.knownHosts],
            ] as const
          ).map(([label, count]) => (
            <li key={label}>
              <b>{count}</b>
              <span>{label}</span>
            </li>
          ))}
        </ul>
      </Modal>
    );
  }

  return (
    <Modal
      title={t('Exportieren')}
      onCancel={guard.request}
      footer={
        <>
          <span className="spacer" />
          <button data-secondary onClick={guard.request} disabled={busy}>
            {t('Abbrechen')}
          </button>
          <button className="primary" onClick={() => void run()} disabled={!ready}>
            {busy ? t('Exportiere…') : t('Speichern unter…')}
          </button>
        </>
      }
    >
      <form
        className="form"
        onSubmit={(event) => {
          event.preventDefault();
          void run();
        }}
      >
        <p className="dialog-lead">
          {t(
            'Alle Hosts mit Bereichen, Gruppen, Anmeldungen und bekannten Zertifikaten in eine Datei – zum Sichern oder für einen anderen Rechner.',
          )}
        </p>
        <label className="check">
          <input type="checkbox" checked={secrets} onChange={(e) => setSecrets(e.target.checked)} />
          <span>
            <b>{t('Passwörter mitnehmen')}</b>
            <small>
              {t(
                'Die ganze Datei wird dann mit einem eigenen Passwort verschlüsselt (Argon2id, XChaCha20-Poly1305).',
              )}
            </small>
          </span>
        </label>
        {secrets && (
          <div className="export-password">
            <label className="field">
              <span>{t('Passwort für die Datei')}</span>
              <input
                type="password"
                data-autofocus
                value={password}
                autoComplete="new-password"
                aria-invalid={password.length > 0 && password.length < 8}
                onChange={(e) => setPassword(e.target.value)}
              />
            </label>
            <label className="field">
              <span>{t('Wiederholen')}</span>
              <input
                type="password"
                value={repeat}
                autoComplete="new-password"
                aria-invalid={mismatch}
                onChange={(e) => setRepeat(e.target.value)}
              />
            </label>
            {/* One line that is always there, so nothing below it jumps while typing. */}
            <p
              className={mismatch ? 'field-error' : 'field-hint'}
              role={mismatch ? 'alert' : undefined}
            >
              {mismatch
                ? t('Stimmt nicht überein.')
                : password.length >= 8 && password === repeat
                  ? t('Passt ✓')
                  : t('Mindestens 8 Zeichen.')}
            </p>
          </div>
        )}
        {!secrets && (
          <p className="field-hint">
            {t(
              'Ohne Geheimnisse ist die Datei lesbares JSON. Hosts mit gespeichertem Passwort fragen nach dem Import wieder beim Verbinden.',
            )}
          </p>
        )}
        {error && (
          <p className="field-error" role="alert">
            {error}
          </p>
        )}
        <button type="submit" hidden />
      </form>
      {guard.dialog}
    </Modal>
  );
}
