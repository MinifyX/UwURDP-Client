import { useState, type ReactNode } from 'react';
import { locale, t, useLanguage } from '../lib/i18n';
import { asSyncFailure, type SyncFailure, type SyncStatus } from '../lib/sync';
import { Modal } from './Modal';

// What both ways of syncing show: UwUSync's pages in SyncSettings, UwULock's
// in LockSync.

/** A failure in words the person can act on. */
export function failureText(failure: SyncFailure): string {
  switch (failure.kind) {
    case 'vault-locked':
      return t('Der Tresor ist gesperrt.');
    case 'password-wrong':
      return t('Das Master-Passwort war falsch.');
    case 'bad-code':
      return t('Das ist kein gültiger Code. Kopiere ihn am besten vollständig.');
    case 'unreachable':
      return t('Der Server ist nicht erreichbar: {reason}', { reason: failure.message });
    case 'refused':
      return t('Der Server hat abgelehnt: {reason}', { reason: failure.message });
    case 'pairing-failed':
      return t('Die Kopplung hat nicht geklappt: {reason}', { reason: failure.message });
    case 'sign-in':
      return t('Die Anmeldung bei UwULock ist abgelaufen. Melde dich neu an.');
    case 'login-refused':
      return t('UwULock lehnt die Anmeldung ab: {reason}', { reason: failure.message });
    case 'keys-lost':
      return t(
        'Der Schlüssel deines UwULock-Kontos für UwU-Apps lässt sich nicht mehr öffnen: Ein offizieller Bitwarden-Client hat die Schlüssel des Kontos ersetzt. Im UwULock-Web-Tresor kannst du ihn neu anlegen.',
      );
    case 'no-key-pair':
      return t(
        'Dein UwULock-Konto ist noch nicht fertig eingerichtet. Melde dich einmal im UwULock-Web-Tresor an.',
      );
    case 'move-check':
      return t(
        'Die Kopie auf UwULock stimmt in {n} Einträgen nicht mit UwUSync überein. UwURDP bleibt deshalb bei UwUSync – versuch es später noch einmal.',
        { n: failure.differences.length },
      );
    default:
      return failure.message;
  }
}

/** "vor 3 Minuten", in the app's language. */
export function ago(ms: number): string {
  const seconds = Math.round((ms - Date.now()) / 1000);
  const format = new Intl.RelativeTimeFormat(locale(), { numeric: 'auto' });
  const abs = Math.abs(seconds);
  if (abs < 45) return t('gerade eben');
  if (abs < 3600) return format.format(Math.round(seconds / 60), 'minute');
  if (abs < 86_400) return format.format(Math.round(seconds / 3600), 'hour');
  return format.format(Math.round(seconds / 86_400), 'day');
}

export async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // The page's clipboard can be off; the code is on screen to type.
  }
}

/**
 * What the last pass did, and what is wrong: the status line under
 * "Verbunden mit …", for either way of syncing.
 */
export function PassDetails({ status }: { status: SyncStatus }) {
  useLanguage();
  const last = status.last;
  const report = last?.report;
  const locked = status.vault !== 'unlocked';

  let line: ReactNode;
  if (locked) {
    line = t('Pausiert, bis der Tresor offen ist.');
  } else if (status.running) {
    line = t('Synchronisiert gerade…');
  } else if (last?.error) {
    line = (
      <span className="field-error">
        {t('Letzter Versuch {when} fehlgeschlagen: {reason}', {
          when: ago(last.atMs),
          reason: last.error,
        })}
      </span>
    );
  } else if (report && last) {
    line = t('Zuletzt {when}: {pulled} geholt, {pushed} gesendet.', {
      when: ago(last.atMs),
      pulled: report.pulled,
      pushed: report.pushed,
    });
  } else if (status.lastSyncMs) {
    line = t('Zuletzt {when}.', { when: ago(status.lastSyncMs) });
  } else {
    line = t('Noch nicht synchronisiert.');
  }

  return (
    <>
      <p className="setting-description">{line}</p>
      {status.withheld.records > 0 && (
        <p className="setting-description field-error" role="alert">
          {t(
            'Der Server liefert nicht den neuesten Stand – er hält Daten zurück oder spielt alte Versionen ein.',
          )}{' '}
          {status.withheld.records === 1
            ? t('1 Eintrag, den ein anderes Gerät hat, fehlt hier oder ist veraltet.')
            : t('{n} Einträge, die andere Geräte haben, fehlen hier oder sind veraltet.', {
                n: status.withheld.records,
              })}{' '}
          {status.withheld.hostKeys &&
            t(
              'Host-Schlüsseln aus dem Sync wird bis dahin nicht vertraut – beim nächsten Verbinden fragt UwURDP wieder nach.',
            )}
        </p>
      )}
      {status.pending > 0 && !locked && (
        <p className="setting-description">
          {status.pending === 1
            ? t('1 Änderung wartet auf den Server.')
            : t('{n} Änderungen warten auf den Server.', { n: status.pending })}
        </p>
      )}
      {report && report.apply.rejected > 0 && (
        <p className="setting-description field-error">
          {t(
            '{n} Einträge vom Server ließen sich nicht öffnen und wurden verworfen. Das sollte nie passieren – prüfe den Server.',
            { n: report.apply.rejected },
          )}
        </p>
      )}
    </>
  );
}

/** The state dot before "Verbunden mit …". */
export function syncDot(status: SyncStatus): 'paused' | 'error' | 'ok' {
  if (status.vault !== 'unlocked') return 'paused';
  if (status.last?.error || status.withheld.records > 0 || status.lock.needsSignIn) return 'error';
  return 'ok';
}

/** A question that needs the master password to go ahead. */
export function PasswordConfirm({
  title,
  lead,
  action,
  tone = 'danger',
  run,
  onCancel,
  onDone,
}: {
  title: string;
  lead: string;
  action: string;
  /** `normal` for a question that changes nothing, only shows something. */
  tone?: 'danger' | 'normal';
  run: (password: string) => Promise<void>;
  onCancel: () => void;
  onDone: () => void;
}) {
  useLanguage();
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async () => {
    if (!password || busy) return;
    setBusy(true);
    setError(null);
    try {
      await run(password);
      setPassword('');
      onDone();
    } catch (e) {
      setError(failureText(asSyncFailure(e)));
      setPassword('');
    } finally {
      setBusy(false);
    }
  };
  return (
    <Modal
      title={title}
      tone={tone === 'danger' ? 'warning' : 'default'}
      onCancel={onCancel}
      footer={
        <>
          <span className="spacer" />
          <button data-autofocus onClick={onCancel} disabled={busy}>
            {t('Abbrechen')}
          </button>
          <button
            className={tone === 'danger' ? 'danger' : 'primary'}
            data-secondary
            disabled={!password || busy}
            onClick={() => void submit()}
          >
            {busy ? t('Einen Moment…') : action}
          </button>
        </>
      }
    >
      <form
        className="form"
        onSubmit={(event) => {
          event.preventDefault();
          void submit();
        }}
      >
        <p className="dialog-lead">{lead}</p>
        <label className="field">
          <span>{t('Master-Passwort')}</span>
          <input
            type="password"
            value={password}
            autoComplete="current-password"
            onChange={(e) => setPassword(e.target.value)}
          />
        </label>
        {error && (
          <p className="field-error" role="alert">
            {error}
          </p>
        )}
        <button type="submit" hidden />
      </form>
    </Modal>
  );
}
