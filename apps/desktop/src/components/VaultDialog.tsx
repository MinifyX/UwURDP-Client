import { useEffect, useRef, useState, type FormEvent, type ReactNode } from 'react';
import { t, useLanguage } from '../lib/i18n';
import { systemName } from '../lib/platform';
import { createVault, repairVault, unlockVault, vaultState } from '../lib/session';
import { Modal } from './Modal';
import { NyuScene } from './nyu/scenes';

type Props = {
  /** Why the vault is needed right now, in one sentence. */
  reason?: ReactNode;
  /** A button label for the dismissive choice; "Abbrechen" by default. */
  cancelLabel?: string;
  onDone: () => void;
  onCancel: () => void;
};

/**
 * Open the vault — or create it, if there is none yet. Either way the user can
 * let this Windows account open it from now on, so the master password is a
 * once-per-device thing (see `uwurdp_store::device`). One dialog for every
 * place that needs the vault: app start, a host with a stored password, the
 * host form, an import, keys.
 *
 * And one more case: a vault a refused sync connect left needing an account
 * key nobody has. If this device still opens it on its own, the dialog asks
 * for a new master password instead (`repair_vault`).
 */
export function VaultDialog({ reason, cancelLabel = t('Abbrechen'), onDone, onCancel }: Props) {
  useLanguage();
  const [mode, setMode] = useState<'loading' | 'create' | 'unlock' | 'repair'>('loading');
  const [password, setPassword] = useState('');
  const [confirm, setConfirm] = useState('');
  const [needsCode, setNeedsCode] = useState(false);
  const [code, setCode] = useState('');
  const [remember, setRemember] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const passwordRef = useRef<HTMLInputElement>(null);

  // The fields appear once the status is known; the cursor goes there then.
  useEffect(() => {
    if (mode !== 'loading') passwordRef.current?.focus();
  }, [mode]);

  useEffect(() => {
    void vaultState()
      .then((state) => {
        if (state.stranded && state.remembered) setMode('repair');
        else if (state.status === 'unlocked') onDone();
        else {
          setNeedsCode(state.needsRecoveryCode);
          setMode(state.status === 'absent' ? 'create' : 'unlock');
        }
      })
      .catch((e) => setError(String(e)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Both ask for a new password, twice.
  const fresh = mode === 'create' || mode === 'repair';
  const mismatch = fresh && confirm.length > 0 && password !== confirm;
  const ready =
    !busy &&
    password.length > 0 &&
    (!needsCode || code.trim().length > 0) &&
    (mode === 'unlock' || (password === confirm && !mismatch));

  const submit = async (event?: FormEvent) => {
    event?.preventDefault();
    if (!ready) return;
    setBusy(true);
    setError(null);
    try {
      if (mode === 'create') await createVault(password, remember);
      else if (mode === 'repair') await repairVault(password, remember);
      else await unlockVault(password, remember, needsCode ? code : null);
      setPassword('');
      setConfirm('');
      onDone();
    } catch (e) {
      const text = String(e);
      setError(
        mode === 'unlock'
          ? text.includes('typo')
            ? t('Im Wiederherstellungscode ist ein Tippfehler.')
            : needsCode
              ? t('Master-Passwort oder Wiederherstellungscode war falsch.')
              : t('Das Master-Passwort war falsch.')
          : t('Hat nicht geklappt: {error}', { error: text }),
      );
      setPassword('');
      passwordRef.current?.focus();
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal
      title={
        mode === 'create'
          ? t('Tresor anlegen')
          : mode === 'repair'
            ? t('Neues Master-Passwort')
            : mode === 'unlock'
              ? t('Tresor entsperren')
              : t('Tresor')
      }
      onCancel={onCancel}
      footer={
        <>
          <span className="spacer" />
          <button data-secondary onClick={onCancel} disabled={busy}>
            {cancelLabel}
          </button>
          <button className="primary" onClick={() => void submit()} disabled={!ready}>
            {busy
              ? mode === 'create'
                ? t('Lege an…')
                : mode === 'repair'
                  ? t('Speichere…')
                  : t('Entsperre…')
              : mode === 'create'
                ? t('Anlegen')
                : mode === 'repair'
                  ? t('Speichern')
                  : t('Entsperren')}
          </button>
        </>
      }
    >
      <NyuScene name="vault" className="dialog-scene" />
      {mode !== 'loading' && (
        <form className="form" onSubmit={submit}>
          <p className="dialog-lead">
            {mode === 'repair'
              ? t(
                  'Ein abgebrochener Sync-Versuch hat deinen Tresor so verschlüsselt, dass das Master-Passwort allein ihn nicht mehr öffnet. Dieses Gerät kommt noch hinein – leg ein neues Master-Passwort fest, dann ist alles wieder wie vorher.',
                )
              : (reason ??
                (mode === 'create'
                  ? t(
                      'Passwörter liegen verschlüsselt im Tresor. Dafür brauchst du einmal ein Master-Passwort.',
                    )
                  : t('Passwörter liegen verschlüsselt im Tresor.')))}
          </p>
          <label className="field">
            <span>{t('Master-Passwort')}</span>
            <input
              ref={passwordRef}
              type="password"
              value={password}
              autoComplete={fresh ? 'new-password' : 'current-password'}
              onChange={(e) => setPassword(e.target.value)}
              aria-invalid={Boolean(error)}
            />
          </label>
          {mode === 'unlock' && needsCode && (
            <label className="field">
              <span>{t('Wiederherstellungscode (aus dem Recovery-Kit)')}</span>
              <input
                value={code}
                autoComplete="off"
                spellCheck={false}
                placeholder="XXXXXXX-XXXXXXX-XXXXXXX-XXXXXXX"
                onChange={(e) => setCode(e.target.value)}
              />
              <em className="field-hint">
                {t(
                  'Dieser Tresor wird synchronisiert und braucht neben dem Passwort den Code, den UwURDP beim Verbinden des Servers einmal gezeigt hat.',
                )}
              </em>
            </label>
          )}
          {fresh && (
            <label className="field">
              <span>{t('Wiederholen')}</span>
              <input
                type="password"
                value={confirm}
                autoComplete="new-password"
                onChange={(e) => setConfirm(e.target.value)}
                aria-invalid={mismatch}
              />
              {mismatch && (
                <em className="field-error">{t('Die Passwörter stimmen nicht überein.')}</em>
              )}
            </label>
          )}
          <label className="check">
            <input
              type="checkbox"
              checked={remember}
              onChange={(e) => setRemember(e.target.checked)}
            />
            <span>
              <b>{t('Auf diesem Gerät merken')}</b>
              <small>
                {t(
                  '{system} öffnet den Tresor für dein Benutzerkonto automatisch – du gibst das Master-Passwort hier nicht noch einmal ein.',
                  { system: systemName() },
                )}
              </small>
            </span>
          </label>
          {error && (
            <p className="field-error" role="alert">
              {error}
            </p>
          )}
          {mode === 'create' && (
            <p className="import-warning">
              {t(
                'Es gibt noch keine Wiederherstellung: Vergisst du das Master-Passwort, kommst du auf einem neuen Gerät nicht mehr an die gespeicherten Passwörter.',
              )}
            </p>
          )}
          <button type="submit" hidden disabled={!ready} />
        </form>
      )}
    </Modal>
  );
}
