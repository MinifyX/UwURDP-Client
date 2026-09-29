import { useState } from 'react';
import { t, useLanguage } from '../lib/i18n';
import {
  asSyncFailure,
  lockLeaveUwusync,
  lockMove,
  lockSendEmailCode,
  lockSignIn,
  lockSignOut,
  syncNow,
  type Difference,
  type LockOutcome,
  type SyncStatus,
  type TwoFactorMethod,
} from '../lib/sync';
import { Icon } from './Icon';
import { NyuScene } from './nyu/scenes';
import { ago, failureText, PassDetails, PasswordConfirm, syncDot } from './SyncParts';
import { VaultDialog } from './VaultDialog';

/** A two-step method, as the person knows it. */
function methodName(method: TwoFactorMethod): string {
  switch (method.kind) {
    case 'authenticator':
      return t('Authenticator-App');
    case 'email':
      return method.hint
        ? t('Code per E-Mail an {address}', { address: method.hint })
        : t('Code per E-Mail');
    case 'yubikey':
      return t('YubiKey (OTP)');
    default:
      return method.kind;
  }
}

/** The records the move's check found missing or out of date, briefly. */
function differencesText(differences: Difference[]): string {
  const missing = differences.filter((d) => d.problem === 'missing').length;
  const older = differences.length - missing;
  return t('{missing} fehlen, {older} sind veraltet.', { missing, older });
}

/**
 * Signing in to UwULock: server, email and master password, then the code of
 * the second step if the account has one. `move` does the same and then the
 * move from UwUSync, which may take a moment longer.
 */
export function LockSignInForm({
  mode,
  status,
  onBack,
  onDone,
}: {
  mode: 'sign-in' | 'move';
  status: SyncStatus;
  onBack: () => void;
  onDone: (outcome: LockOutcome) => void;
}) {
  useLanguage();
  const [server, setServer] = useState(status.lock.serverUrl ?? '');
  const [email, setEmail] = useState(status.lock.email ?? '');
  const [password, setPassword] = useState('');
  const [methods, setMethods] = useState<TwoFactorMethod[] | null>(null);
  const [provider, setProvider] = useState<number | null>(null);
  const [code, setCode] = useState('');
  const [remember, setRemember] = useState(true);
  const [mailed, setMailed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [differences, setDifferences] = useState<Difference[] | null>(null);
  const [unlocking, setUnlocking] = useState(false);

  const usable = methods?.filter((m) => m.supported) ?? [];
  const chosen = usable.find((m) => m.provider === provider) ?? usable[0] ?? null;
  const ready =
    !busy &&
    server.trim().length > 0 &&
    email.trim().length > 0 &&
    password.length > 0 &&
    (methods === null || (chosen !== null && code.trim().length > 0));

  const submit = async () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    setDifferences(null);
    const twoFactor =
      methods && chosen ? { provider: chosen.provider, code: code.trim(), remember } : null;
    try {
      const run = mode === 'move' ? lockMove : lockSignIn;
      const outcome = await run(server.trim(), email.trim(), password, twoFactor);
      if (outcome.kind === 'two-factor') {
        setMethods(outcome.methods);
        setCode('');
        if (outcome.message) setError(outcome.message);
        else if (!outcome.methods.some((m) => m.supported)) {
          setError(
            t(
              'Dein Konto nutzt nur Zwei-Faktor-Wege, die UwURDP noch nicht kann. Richte im UwULock-Web-Tresor eine Authenticator-App oder Codes per E-Mail ein.',
            ),
          );
        }
        return;
      }
      setPassword('');
      setCode('');
      onDone(outcome);
    } catch (e) {
      const failure = asSyncFailure(e);
      if (failure.kind === 'vault-locked') {
        setUnlocking(true);
        return;
      }
      if (failure.kind === 'move-check') setDifferences(failure.differences);
      setError(failureText(failure));
      // A code is used up by trying it; the password stays for the next try.
      setCode('');
    } finally {
      setBusy(false);
    }
  };

  const sendMail = async () => {
    setError(null);
    try {
      await lockSendEmailCode(server.trim(), email.trim(), password);
      setMailed(true);
    } catch (e) {
      setError(failureText(asSyncFailure(e)));
    }
  };

  return (
    <form
      className="form sync-form"
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <p className="setting-label">
        {mode === 'move' ? t('Zu UwULock umziehen') : t('Mit UwULock anmelden')}
      </p>
      <p className="dialog-lead">
        {mode === 'move'
          ? t(
              'UwURDP meldet sich bei UwULock an, kopiert alles von UwUSync in dein UwULock-Konto, prüft die Kopie und wechselt erst dann. Auf UwUSync bleibt alles, wie es ist. Danach öffnet das Master-Passwort deines UwULock-Kontos den Tresor hier.',
            )
          : t(
              'Hosts und Passwörter auf diesem Gerät kommen mit in dein Konto. Danach öffnet das Master-Passwort deines UwULock-Kontos den Tresor hier.',
            )}
      </p>
      {methods === null ? (
        <>
          <label className="field">
            <span>{t('Server')}</span>
            <input
              value={server}
              autoFocus={server.length === 0}
              spellCheck={false}
              placeholder="https://lock.example.com"
              onChange={(e) => setServer(e.target.value)}
            />
          </label>
          <label className="field">
            <span>{t('E-Mail')}</span>
            <input
              type="email"
              value={email}
              autoComplete="username"
              spellCheck={false}
              onChange={(e) => setEmail(e.target.value)}
            />
          </label>
          <label className="field">
            <span>{t('Master-Passwort')}</span>
            <input
              type="password"
              value={password}
              autoFocus={server.length > 0}
              autoComplete="current-password"
              onChange={(e) => setPassword(e.target.value)}
            />
          </label>
        </>
      ) : (
        <>
          <p className="field-hint">
            {t('Dein Konto hat eine Zwei-Faktor-Anmeldung. Gib den Code ein.')}
          </p>
          {usable.length > 1 && (
            <label className="field">
              <span>{t('Weg')}</span>
              <select
                className="select"
                value={chosen?.provider ?? ''}
                onChange={(e) => {
                  setProvider(Number(e.target.value));
                  setMailed(false);
                }}
              >
                {usable.map((method) => (
                  <option key={method.provider} value={method.provider}>
                    {methodName(method)}
                  </option>
                ))}
              </select>
            </label>
          )}
          {usable.length === 1 && chosen && <p className="field-hint">{methodName(chosen)}</p>}
          {chosen && (
            <label className="field">
              <span>{t('Code')}</span>
              <input
                value={code}
                autoFocus
                autoComplete="one-time-code"
                inputMode={chosen.kind === 'yubikey' ? 'text' : 'numeric'}
                spellCheck={false}
                onChange={(e) => setCode(e.target.value)}
              />
            </label>
          )}
          {chosen?.kind === 'email' && (
            <button type="button" onClick={() => void sendMail()} disabled={busy}>
              <Icon name={mailed ? 'check' : 'refresh'} size={15} />
              {mailed ? t('Code ist unterwegs') : t('Code per E-Mail schicken')}
            </button>
          )}
          <label className="check">
            <input
              type="checkbox"
              checked={remember}
              onChange={(e) => setRemember(e.target.checked)}
            />
            <span>
              <b>{t('Dieses Gerät merken')}</b>
              <small>{t('Beim nächsten Anmelden fragt UwULock hier nicht nach dem Code.')}</small>
            </span>
          </label>
        </>
      )}
      {error && (
        <p className="field-error" role="alert">
          {error}
          {differences && differences.length > 0 && <> {differencesText(differences)}</>}
        </p>
      )}
      {busy && mode === 'move' && (
        <p className="field-hint" role="status">
          {t('Zieht um… das dauert bei vielen Einträgen einen Moment.')}
        </p>
      )}
      <div className="sync-actions">
        <button
          type="button"
          data-secondary
          onClick={() => {
            if (methods) {
              setMethods(null);
              setError(null);
            } else onBack();
          }}
          disabled={busy}
        >
          {t('Zurück')}
        </button>
        <span className="spacer" />
        <button type="submit" className="primary" disabled={!ready}>
          {busy
            ? mode === 'move'
              ? t('Zieht um…')
              : t('Melde an…')
            : mode === 'move'
              ? t('Umziehen')
              : t('Anmelden')}
        </button>
      </div>
      {unlocking && (
        <VaultDialog
          reason={t(
            'Zuerst den Tresor dieses Geräts öffnen: seine Passwörter ziehen mit in dein UwULock-Konto.',
          )}
          onDone={() => {
            setUnlocking(false);
            void submit();
          }}
          onCancel={() => setUnlocking(false)}
        />
      )}
    </form>
  );
}

/**
 * Right after the move: what was copied, and whether this device should be
 * removed from UwUSync.
 */
export function MoveDone({
  outcome,
  leftBehind,
  onDone,
}: {
  outcome: Extract<LockOutcome, { kind: 'moved' }>;
  leftBehind: boolean;
  onDone: () => void;
}) {
  useLanguage();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { report } = outcome;

  const leave = async (revoke: boolean) => {
    setBusy(true);
    setError(null);
    try {
      await lockLeaveUwusync(revoke);
      onDone();
    } catch (e) {
      setError(failureText(asSyncFailure(e)));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="sync-kit">
      <NyuScene name="done" className="sync-scene" />
      <p className="setting-label">{t('Umgezogen ✧ UwURDP synchronisiert jetzt über UwULock')}</p>
      <p className="dialog-lead">
        {t(
          '{read} Einträge von UwUSync sind in deinem UwULock-Konto, geprüft: {copied} jetzt kopiert, {there} waren schon da.',
          {
            read: report.read,
            copied: report.copied,
            there: report.alreadyThere + report.newerThere,
          },
        )}{' '}
        {t('Der Tresor hier öffnet ab jetzt mit dem Master-Passwort deines UwULock-Kontos.')}
      </p>
      {report.unreadable > 0 && (
        <p className="import-warning">
          {t(
            '{n} Einträge auf UwUSync ließen sich mit dem Schlüssel dieses Geräts nicht öffnen und sind nicht umgezogen.',
            { n: report.unreadable },
          )}
        </p>
      )}
      {leftBehind && outcome.lastDevice ? (
        // UwUSync keeps its last device (it refuses to remove it): nothing
        // to leave, only an account its admin can delete now.
        <>
          <p className="setting-description">
            {t(
              'Das war das letzte Gerät auf UwUSync: Wer den Server betreibt, kann das Konto dort jetzt löschen.',
            )}
          </p>
          <div className="sync-actions">
            <span className="spacer" />
            <button className="primary" disabled={busy} onClick={() => void leave(false)}>
              {t('Fertig')}
            </button>
          </div>
        </>
      ) : leftBehind ? (
        <>
          <p className="setting-description">
            {t(
              'Auf UwUSync bleiben deine Daten unberührt. Soll dieses Gerät dort ausgetragen werden? Die anderen Geräte synchronisieren dort weiter, bis auch sie umgezogen sind.',
            )}
          </p>
          {error && (
            <p className="field-error" role="alert">
              {error}
            </p>
          )}
          <div className="sync-actions">
            <button data-secondary disabled={busy} onClick={() => void leave(false)}>
              {t('Eingetragen lassen')}
            </button>
            <span className="spacer" />
            <button className="primary" disabled={busy} onClick={() => void leave(true)}>
              {t('Bei UwUSync austragen')}
            </button>
          </div>
        </>
      ) : (
        <div className="sync-actions">
          <span className="spacer" />
          <button className="primary" onClick={onDone}>
            {t('Fertig')}
          </button>
        </div>
      )}
    </div>
  );
}

/** Settings → Sync while this device syncs through UwULock. */
export function LockOverview({ status, onChanged }: { status: SyncStatus; onChanged: () => void }) {
  useLanguage();
  const [signingIn, setSigningIn] = useState(false);
  const [leaving, setLeaving] = useState(false);
  const [unlocking, setUnlocking] = useState(false);
  const locked = status.vault !== 'unlocked';
  const { lock } = status;

  if (signingIn) {
    return (
      <LockSignInForm
        mode="sign-in"
        status={status}
        onBack={() => setSigningIn(false)}
        onDone={() => {
          setSigningIn(false);
          onChanged();
        }}
      />
    );
  }

  return (
    <>
      <div className="setting-row">
        <div className="setting-text">
          <p className="setting-label">
            <span className="sync-dot" data-state={syncDot(status)} />
            {t('Verbunden mit UwULock {server}', { server: lock.serverUrl ?? '' })}
          </p>
          {lock.email && <p className="setting-description">{lock.email}</p>}
          {lock.needsSignIn ? (
            <p className="setting-description field-error" role="alert">
              {t(
                'Die Anmeldung ist abgelaufen oder wurde beendet. Bis du dich neu anmeldest, bleiben Änderungen auf diesem Gerät.',
              )}
            </p>
          ) : (
            <>
              <PassDetails status={status} />
              {!locked && (
                <p className="setting-description">
                  {lock.live
                    ? t('Live: Änderungen anderer Geräte kommen sofort an.')
                    : lock.liveRefused
                      ? t(
                          'Der Server bietet keine Live-Verbindung an – UwURDP fragt regelmäßig nach.',
                        )
                      : t('Ohne Live-Verbindung – UwURDP fragt jede Minute nach.')}
                </p>
              )}
            </>
          )}
        </div>
        <div className="setting-control">
          {lock.needsSignIn ? (
            <button className="primary" onClick={() => setSigningIn(true)}>
              <Icon name="lock" size={15} />
              {t('Neu anmelden…')}
            </button>
          ) : locked ? (
            <button onClick={() => setUnlocking(true)}>
              <Icon name="unlock" size={15} />
              {t('Entsperren')}
            </button>
          ) : (
            <button
              onClick={() => {
                void syncNow().then(() => window.setTimeout(onChanged, 400));
              }}
              disabled={status.running}
            >
              <Icon name="refresh" size={15} />
              {t('Jetzt synchronisieren')}
            </button>
          )}
        </div>
      </div>

      <div className="setting-row">
        <div className="setting-text">
          <p className="setting-label">{t('Geräte')}</p>
          <p className="setting-description">
            {t(
              'Ein weiteres Gerät meldet sich einfach mit demselben UwULock-Konto an. Deine Geräte siehst und entfernst du im UwULock-Web-Tresor.',
            )}
          </p>
          {lock.signedInMs && (
            <p className="setting-description">
              {t('Angemeldet {when}.', { when: ago(lock.signedInMs) })}
            </p>
          )}
        </div>
      </div>

      <div className="setting-row">
        <div className="setting-text">
          <p className="setting-label">{t('Abmelden')}</p>
          <p className="setting-description">
            {t(
              'Dieses Gerät synchronisiert nicht mehr. Hosts und Tresor bleiben hier, das Master-Passwort deines UwULock-Kontos öffnet ihn weiter.',
            )}
          </p>
        </div>
        <div className="setting-control">
          <button className="danger" onClick={() => setLeaving(true)}>
            {t('Abmelden…')}
          </button>
        </div>
      </div>

      {leaving && (
        <PasswordConfirm
          title={t('Bei UwULock abmelden?')}
          lead={t(
            'Die anderen Geräte synchronisieren weiter. Wieder anmelden geht jederzeit unter Einstellungen → Sync.',
          )}
          action={t('Abmelden')}
          run={(password) => lockSignOut(password)}
          onCancel={() => setLeaving(false)}
          onDone={() => {
            setLeaving(false);
            onChanged();
          }}
        />
      )}
      {unlocking && (
        <VaultDialog
          reason={t('Der Sync braucht den offenen Tresor, um zu ver- und entschlüsseln.')}
          onDone={() => {
            setUnlocking(false);
            onChanged();
            void syncNow();
          }}
          onCancel={() => setUnlocking(false)}
        />
      )}
    </>
  );
}
