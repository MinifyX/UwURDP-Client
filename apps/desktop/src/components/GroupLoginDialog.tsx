import { useState, type FormEvent } from 'react';
import { t, useLanguage } from '../lib/i18n';
import {
  setGroupLogin,
  type GroupRecord,
  type PasswordChange,
  type SaveFailure,
} from '../lib/session';
import { splitLogin } from './ConnectDialogs';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { VaultDialog } from './VaultDialog';

type Props = {
  group: GroupRecord;
  onSaved: () => void;
  onCancel: () => void;
};

/**
 * The login a group hands to its hosts — RDCMan's "inherit from parent" for
 * logon credentials. Every host in the group without a login of its own
 * connects with it.
 */
export function GroupLoginDialog({ group, onSaved, onCancel }: Props) {
  useLanguage();
  const [username, setUsername] = useState(group.username);
  const [domain, setDomain] = useState(group.domain);
  const [password, setPassword] = useState<string | null>(group.hasPassword ? null : '');
  const [forget, setForget] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [vault, setVault] = useState(false);

  const change = (): PasswordChange => {
    if (forget) return { kind: 'forget' };
    if (password === null || password === '') return { kind: 'keep' };
    return { kind: 'set', value: password };
  };

  const submit = async (event?: FormEvent) => {
    event?.preventDefault();
    const login = splitLogin(username, domain);
    setBusy(true);
    setError(null);
    try {
      await setGroupLogin(group.workspace, group.name, login.username, login.domain, change());
      setPassword(null);
      onSaved();
    } catch (raw) {
      const failure = raw as SaveFailure;
      if (failure?.kind === 'vault-locked') setVault(true);
      else if (failure?.kind === 'invalid')
        setError(
          failure.field === 'username'
            ? t('Zum Passwort gehört ein Benutzer')
            : failure.field === 'domain'
              ? t('Die Domäne darf keine Leerzeichen enthalten')
              : failure.problem,
        );
      else setError(failure?.kind === 'error' ? failure.message : String(raw));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      <Modal
        title={t('Anmeldung für {group}', { group: group.name })}
        onCancel={onCancel}
        footer={
          <>
            {group.username && (
              <button
                className="danger"
                data-secondary
                disabled={busy}
                onClick={() => {
                  setUsername('');
                  setDomain('');
                  setForget(true);
                  void setGroupLogin(group.workspace, group.name, '', '', { kind: 'forget' })
                    .then(onSaved)
                    .catch((e) => setError(String(e)));
                }}
              >
                {t('Entfernen')}
              </button>
            )}
            <span className="spacer" />
            <button data-secondary onClick={onCancel} disabled={busy}>
              {t('Abbrechen')}
            </button>
            <button className="primary" onClick={() => void submit()} disabled={busy}>
              {t('Speichern')}
            </button>
          </>
        }
      >
        <form className="form" onSubmit={submit}>
          <p className="dialog-lead">
            {t(
              'Jeder Host in dieser Gruppe ohne eigenen Benutzer meldet sich hiermit an – wie „Von übergeordnetem Element erben“ im RDCMan.',
            )}
          </p>
          <div className="form-row">
            <label className="field grow">
              <span>{t('Benutzer')}</span>
              <input
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                placeholder={t('Administrator')}
                autoComplete="off"
                spellCheck={false}
              />
            </label>
            <label className="field grow">
              <span>{t('Domäne')}</span>
              <input
                value={domain}
                onChange={(e) => setDomain(e.target.value)}
                placeholder={t('optional')}
                autoComplete="off"
                spellCheck={false}
              />
            </label>
          </div>
          {group.hasPassword && !forget && password === null ? (
            <div className="stored-secret">
              <Icon name="lock" size={15} />
              <span>{t('Das Passwort ist im Tresor gespeichert.')}</span>
              <span className="spacer" />
              <button type="button" className="quiet" onClick={() => setPassword('')}>
                {t('Ändern')}
              </button>
              <button type="button" className="quiet" onClick={() => setForget(true)}>
                {t('Entfernen')}
              </button>
            </div>
          ) : forget ? (
            <div className="stored-secret" data-forgotten>
              <Icon name="unlock" size={15} />
              <span>{t('Das gespeicherte Passwort wird beim Speichern entfernt.')}</span>
              <span className="spacer" />
              <button type="button" className="quiet" onClick={() => setForget(false)}>
                {t('Rückgängig')}
              </button>
            </div>
          ) : (
            <label className="field">
              <span>{t('Passwort')}</span>
              <input
                type="password"
                value={password ?? ''}
                onChange={(e) => setPassword(e.target.value)}
                placeholder={t('leer lassen: beim Verbinden fragen')}
                autoComplete="new-password"
              />
              <em className="field-hint">{t('Verschlüsselt im Tresor.')}</em>
            </label>
          )}
          {error && <p className="form-error">{error}</p>}
          <button type="submit" hidden />
        </form>
      </Modal>
      {vault && (
        <VaultDialog
          reason={t('Das Passwort wird verschlüsselt im Tresor gespeichert.')}
          onDone={() => {
            setVault(false);
            void submit();
          }}
          onCancel={() => setVault(false)}
        />
      )}
    </>
  );
}
