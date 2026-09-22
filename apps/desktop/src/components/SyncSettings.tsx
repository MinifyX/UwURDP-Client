import { listen } from '@tauri-apps/api/event';
import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { locale, t, useLanguage } from '../lib/i18n';
import {
  asSyncFailure,
  isPasteablePairing,
  syncCancelOffer,
  syncConnect,
  syncDevices,
  syncDisconnect,
  syncJoin,
  syncNow,
  syncOffer,
  syncRecoveryCode,
  syncRevoke,
  syncStatus,
  syncWaitForDevice,
  type Connected,
  type Device,
  type Offer,
  type SyncFailure,
  type SyncStatus,
} from '../lib/sync';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { NyuScene } from './nyu/scenes';
import { VaultDialog } from './VaultDialog';

/** A failure in words the person can act on. */
function failureText(failure: SyncFailure): string {
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
    default:
      return failure.message;
  }
}

/** "vor 3 Minuten", in the app's language. */
function ago(ms: number): string {
  const seconds = Math.round((ms - Date.now()) / 1000);
  const format = new Intl.RelativeTimeFormat(locale(), { numeric: 'auto' });
  const abs = Math.abs(seconds);
  if (abs < 45) return t('gerade eben');
  if (abs < 3600) return format.format(Math.round(seconds / 60), 'minute');
  if (abs < 86_400) return format.format(Math.round(seconds / 3600), 'hour');
  return format.format(Math.round(seconds / 86_400), 'day');
}

async function copy(text: string) {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    // The page's clipboard can be off; the code is on screen to type.
  }
}

/**
 * Settings → Sync. Not paired: connect a server (first device) or join from
 * another one. Paired: what the last pass did, adding a device, the device
 * list with revoking, and leaving.
 */
export function SyncSettings() {
  useLanguage();
  const [status, setStatus] = useState<SyncStatus | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [mode, setMode] = useState<'overview' | 'connect' | 'join'>('overview');
  const [kit, setKit] = useState<Connected | null>(null);

  const refresh = useCallback(() => {
    void syncStatus()
      .then(setStatus)
      .catch((e) => setError(String(e)));
  }, []);

  useEffect(() => {
    refresh();
    const stop = listen('sync:status', refresh);
    // "vor 2 Minuten" moves on by itself.
    const timer = window.setInterval(refresh, 20_000);
    return () => {
      window.clearInterval(timer);
      void stop.then((unlisten) => unlisten());
    };
  }, [refresh]);

  if (!status) {
    return <p className="setting-description">{error ?? t('Wird geladen…')}</p>;
  }

  if (kit) {
    return (
      <RecoveryKit
        kit={kit}
        onDone={() => {
          setKit(null);
          setMode('overview');
          refresh();
        }}
      />
    );
  }

  if (!status.paired) {
    if (mode === 'connect') {
      return (
        <ConnectForm
          status={status}
          onBack={() => setMode('overview')}
          onConnected={(connected) => setKit(connected)}
        />
      );
    }
    if (mode === 'join') {
      return (
        <JoinForm
          status={status}
          onBack={() => setMode('overview')}
          onJoined={() => {
            setMode('overview');
            refresh();
          }}
        />
      );
    }
    return (
      <div className="sync-intro">
        <NyuScene name="welcome" className="sync-scene" />
        <p className="dialog-lead">
          {t(
            'Mit einem eigenen UwUSync-Server bleiben Hosts, Gruppen, Keys und Passwörter auf allen deinen Geräten gleich. Der Server sieht davon nur verschlüsselte Blöcke.',
          )}
        </p>
        <div className="sync-choices">
          <button className="sync-choice" onClick={() => setMode('connect')}>
            <Icon name="network" size={20} />
            <span>
              <b>{t('Server verbinden')}</b>
              <small>
                {t(
                  'Das erste Gerät: mit dem Einrichtungscode, den der Server beim ersten Start zeigt.',
                )}
              </small>
            </span>
          </button>
          <button className="sync-choice" onClick={() => setMode('join')}>
            <Icon name="plus" size={20} />
            <span>
              <b>{t('Mit einem Gerät koppeln')}</b>
              <small>
                {t(
                  'Ein weiteres Gerät: mit dem Code, den ein schon verbundenes Gerät unter „Gerät hinzufügen“ zeigt.',
                )}
              </small>
            </span>
          </button>
        </div>
        <p className="setting-description">
          {t(
            'Einen Server aufsetzen: siehe github.com/MinifyX/UwUSync-Server – ein Befehl, Docker, fertig.',
          )}
        </p>
      </div>
    );
  }

  return <Paired status={status} onChanged={refresh} />;
}

// ── First device ──────────────────────────────────────────────────────────

function ConnectForm({
  status,
  onBack,
  onConnected,
}: {
  status: SyncStatus;
  onBack: () => void;
  onConnected: (connected: Connected) => void;
}) {
  useLanguage();
  const fresh = status.vault === 'absent';
  const [code, setCode] = useState('');
  const [name, setName] = useState(status.deviceName);
  const [password, setPassword] = useState('');
  const [repeat, setRepeat] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const mismatch = fresh && repeat.length > 0 && password !== repeat;
  const ready =
    !busy &&
    code.trim().startsWith('uwu1_') &&
    name.trim().length > 0 &&
    password.length > 0 &&
    (!fresh || (password.length >= 8 && password === repeat));

  const submit = async () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    try {
      const connected = await syncConnect(code, password, name);
      setPassword('');
      setRepeat('');
      onConnected(connected);
    } catch (e) {
      setError(failureText(asSyncFailure(e)));
      setPassword('');
      setRepeat('');
    } finally {
      setBusy(false);
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
      <p className="setting-label">{t('Server verbinden')}</p>
      <label className="field">
        <span>{t('Einrichtungscode')}</span>
        <textarea
          className="sync-code-input"
          value={code}
          rows={3}
          spellCheck={false}
          autoFocus
          placeholder="uwu1_…"
          onChange={(e) => setCode(e.target.value)}
        />
        <em className="field-hint">
          {t(
            'Steht im Log des Servers, oder: docker compose exec uwusync uwusync-server invite. Er enthält Adresse, Zertifikat-Fingerprint und eine Einladung.',
          )}
        </em>
      </label>
      <label className="field">
        <span>{t('Name dieses Geräts')}</span>
        <input value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
      </label>
      <label className="field">
        <span>{fresh ? t('Neues Master-Passwort') : t('Master-Passwort deines Tresors')}</span>
        <input
          type="password"
          value={password}
          autoComplete={fresh ? 'new-password' : 'current-password'}
          onChange={(e) => setPassword(e.target.value)}
        />
      </label>
      {fresh && (
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
      )}
      <p className={mismatch ? 'field-error' : 'field-hint'}>
        {mismatch
          ? t('Die Passwörter stimmen nicht überein.')
          : fresh
            ? t('Mindestens 8 Zeichen. Es verlässt dieses Gerät nie.')
            : t('Deine Hosts und Passwörter kommen mit – nichts wird neu verschlüsselt.')}
      </p>
      {error && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
      <div className="sync-actions">
        <button type="button" data-secondary onClick={onBack} disabled={busy}>
          {t('Zurück')}
        </button>
        <span className="spacer" />
        <button type="submit" className="primary" disabled={!ready}>
          {busy ? t('Verbinde…') : t('Verbinden')}
        </button>
      </div>
    </form>
  );
}

/** Shown right after the account was made, and again when asked for. */
function RecoveryKit({
  kit,
  again = false,
  onDone,
}: {
  kit: Connected;
  again?: boolean;
  onDone: () => void;
}) {
  useLanguage();
  const [saved, setSaved] = useState(false);
  const [copied, setCopied] = useState(false);
  const text = [
    'UwURDP Recovery-Kit',
    `Server: ${kit.serverUrl}`,
    kit.tlsFingerprint ? `Fingerprint: ${kit.tlsFingerprint}` : null,
    `Code: ${kit.recoveryCode}`,
  ]
    .filter(Boolean)
    .join('\n');
  return (
    <div className="sync-kit">
      <NyuScene name="keys" className="sync-scene" />
      <p className="setting-label">
        {again ? t('Dein Recovery-Kit') : t('Verbunden ✧ Jetzt das Recovery-Kit sichern')}
      </p>
      <p className="dialog-lead">
        {again
          ? t(
              'Dein Tresor braucht das Master-Passwort und diesen Code. Deine Geräte merken sich den Code – aber sind alle Geräte weg und der Code auch, sind die Daten weg.',
            )
          : t(
              'Dein Tresor braucht ab jetzt das Master-Passwort und diesen Code. Deine Geräte merken sich den Code – aber sind alle Geräte weg und der Code auch, sind die Daten weg. Später zeigt ihn jedes gekoppelte Gerät unter Sync → Recovery-Kit noch einmal.',
            )}
      </p>
      <div className="sync-kit-card">
        <code className="sync-kit-code">{kit.recoveryCode}</code>
        <dl>
          <dt>{t('Server')}</dt>
          <dd>
            <code>{kit.serverUrl}</code>
          </dd>
          {kit.tlsFingerprint && (
            <>
              <dt>{t('Fingerprint')}</dt>
              <dd>
                <code>{kit.tlsFingerprint}</code>
              </dd>
            </>
          )}
        </dl>
        <button
          onClick={() => {
            void copy(text);
            setCopied(true);
          }}
        >
          <Icon name={copied ? 'check' : 'copy'} size={15} />
          {copied ? t('Kopiert') : t('Kopieren')}
        </button>
      </div>
      {/* Asked for again, the kit was saved once already: no gate. */}
      {!again && (
        <label className="check">
          <input type="checkbox" checked={saved} onChange={(e) => setSaved(e.target.checked)} />
          <span>
            <b>{t('Ich habe den Code sicher aufgeschrieben')}</b>
            <small>
              {t('Auf Papier oder im Passwort-Manager – nicht nur auf diesem Rechner.')}
            </small>
          </span>
        </label>
      )}
      <div className="sync-actions">
        <span className="spacer" />
        <button className="primary" disabled={!saved && !again} onClick={onDone}>
          {t('Fertig')}
        </button>
      </div>
    </div>
  );
}

// ── Joining from another device ───────────────────────────────────────────

function JoinForm({
  status,
  onBack,
  onJoined,
}: {
  status: SyncStatus;
  onBack: () => void;
  onJoined: () => void;
}) {
  useLanguage();
  const [code, setCode] = useState('');
  const [server, setServer] = useState('');
  const [fingerprint, setFingerprint] = useState('');
  const [name, setName] = useState(status.deviceName);
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [unlocking, setUnlocking] = useState(false);

  const pasted = isPasteablePairing(code);
  const spoken = code.trim().length > 0 && !pasted;
  const ready =
    !busy &&
    code.trim().length > 0 &&
    (!spoken || server.trim().length > 0) &&
    name.trim().length > 0 &&
    password.length > 0;

  const submit = async () => {
    if (!ready) return;
    setBusy(true);
    setError(null);
    try {
      await syncJoin(
        code,
        password,
        name,
        spoken ? server : null,
        spoken && fingerprint.trim() ? fingerprint : null,
      );
      setPassword('');
      onJoined();
    } catch (e) {
      const failure = asSyncFailure(e);
      if (failure.kind === 'vault-locked') setUnlocking(true);
      else {
        setError(failureText(failure));
        setPassword('');
      }
    } finally {
      setBusy(false);
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
      <p className="setting-label">{t('Mit einem Gerät koppeln')}</p>
      <p className="setting-description">
        {t(
          'Auf dem anderen Gerät: Einstellungen → Sync → Gerät hinzufügen. Den langen Code dort kopieren und hier einfügen – oder den kurzen Code abtippen.',
        )}
      </p>
      <label className="field">
        <span>{t('Code vom anderen Gerät')}</span>
        <textarea
          className="sync-code-input"
          value={code}
          rows={2}
          spellCheck={false}
          autoFocus
          placeholder="uwu2_…  ·  K7M4Q-tiger-radio-kiwi"
          onChange={(e) => setCode(e.target.value)}
        />
      </label>
      {spoken && (
        <>
          <label className="field">
            <span>{t('Server-Adresse')}</span>
            <input
              value={server}
              spellCheck={false}
              placeholder="https://nas.lan:8443"
              onChange={(e) => setServer(e.target.value)}
            />
          </label>
          <label className="field">
            <span>{t('Fingerprint (steht beim anderen Gerät)')}</span>
            <input
              value={fingerprint}
              spellCheck={false}
              placeholder="SHA256:…"
              onChange={(e) => setFingerprint(e.target.value)}
            />
          </label>
        </>
      )}
      <label className="field">
        <span>{t('Name dieses Geräts')}</span>
        <input value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
      </label>
      <label className="field">
        <span>{t('Master-Passwort des Kontos')}</span>
        <input
          type="password"
          value={password}
          autoComplete="current-password"
          onChange={(e) => setPassword(e.target.value)}
        />
        <em className="field-hint">
          {t(
            'Das Passwort, mit dem das erste Gerät den Server verbunden hat. Hosts, die schon hier sind, kommen mit.',
          )}
        </em>
      </label>
      {busy && (
        <p className="field-hint" role="status">
          {t('Warte auf das andere Gerät…')}
        </p>
      )}
      {error && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
      <div className="sync-actions">
        <button type="button" data-secondary onClick={onBack} disabled={busy}>
          {t('Zurück')}
        </button>
        <span className="spacer" />
        <button type="submit" className="primary" disabled={!ready}>
          {busy ? t('Kopple…') : t('Koppeln')}
        </button>
      </div>
      {unlocking && (
        <VaultDialog
          reason={t(
            'Zuerst den Tresor dieses Geräts öffnen: seine Passwörter ziehen mit in den Tresor des Kontos.',
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

// ── Paired ────────────────────────────────────────────────────────────────

function Paired({ status, onChanged }: { status: SyncStatus; onChanged: () => void }) {
  useLanguage();
  const [devices, setDevices] = useState<Device[] | null>(null);
  const [devicesError, setDevicesError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [offer, setOffer] = useState<Offer | null>(null);
  const [joined, setJoined] = useState<string | null>(null);
  const [revoking, setRevoking] = useState<Device | null>(null);
  const [revoked, setRevoked] = useState<string | null>(null);
  const [leaving, setLeaving] = useState(false);
  const [unlocking, setUnlocking] = useState(false);
  const [askingKit, setAskingKit] = useState(false);
  const [kit, setKit] = useState<Connected | null>(null);

  const loadDevices = useCallback(() => {
    void syncDevices()
      .then((list) => {
        setDevices(list);
        setDevicesError(null);
      })
      .catch((e) => setDevicesError(failureText(asSyncFailure(e))));
  }, []);

  useEffect(() => {
    if (status.vault === 'unlocked') loadDevices();
  }, [loadDevices, status.vault]);

  const last = status.last;
  const report = last?.report;
  const locked = status.vault !== 'unlocked';

  if (kit) return <RecoveryKit kit={kit} again onDone={() => setKit(null)} />;

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
      <div className="setting-row">
        <div className="setting-text">
          <p className="setting-label">
            <span
              className="sync-dot"
              data-state={
                locked ? 'paused' : last?.error || status.withheld.records > 0 ? 'error' : 'ok'
              }
            />
            {t('Verbunden mit {server}', { server: status.serverUrl ?? '' })}
          </p>
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
          {status.tlsFingerprint && (
            <p className="setting-description">
              {t('Gepinnt:')} <code className="sync-fingerprint">{status.tlsFingerprint}</code>
            </p>
          )}
        </div>
        <div className="setting-control">
          {locked ? (
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
          <p className="setting-label">{t('Gerät hinzufügen')}</p>
          <p className="setting-description">
            {t(
              'Zeigt einen Code für zehn Minuten. Das neue Gerät gibt ihn unter Sync → Mit einem Gerät koppeln ein.',
            )}
          </p>
          {joined && (
            <p className="setting-result" role="status">
              {t('{name} ist beigetreten ✧', { name: joined })}
            </p>
          )}
        </div>
        <div className="setting-control">
          <button
            onClick={() => {
              setJoined(null);
              setAdding(true);
            }}
            disabled={locked}
          >
            <Icon name="plus" size={15} />
            {t('Gerät hinzufügen…')}
          </button>
        </div>
      </div>

      <div className="sync-devices">
        <p className="setting-label">{t('Geräte')}</p>
        {devicesError && <p className="setting-description field-error">{devicesError}</p>}
        {locked && (
          <p className="setting-description">{t('Die Liste kommt, wenn der Tresor offen ist.')}</p>
        )}
        {devices && (
          <ul className="sync-device-list">
            {devices.map((device) => (
              <li key={device.id} data-revoked={device.revokedMs ? true : undefined}>
                <Icon name="network" size={16} />
                <span className="sync-device-text">
                  <b>
                    {device.name}
                    {device.current && <span className="rule-tag">{t('dieses Gerät')}</span>}
                  </b>
                  <small>
                    {device.revokedMs
                      ? t('Widerrufen {when}', { when: ago(device.revokedMs) })
                      : device.lastSeenMs
                        ? t('Zuletzt gesehen {when}', { when: ago(device.lastSeenMs) })
                        : t('Gekoppelt {when}', { when: ago(device.createdMs) })}
                  </small>
                </span>
                <span className="spacer" />
                {!device.current && !device.revokedMs && (
                  <button className="quiet" onClick={() => setRevoking(device)}>
                    {t('Widerrufen…')}
                  </button>
                )}
              </li>
            ))}
          </ul>
        )}
        {revoked && (
          <p className="import-warning" role="status">
            {t(
              '{name} kommt nicht mehr an den Server. Was es schon heruntergeladen hat, kennt es aber weiter: ändere das Master-Passwort und die Passwörter wichtiger Hosts, wenn das Gerät in falsche Hände geraten ist.',
              { name: revoked },
            )}
          </p>
        )}
      </div>

      <div className="setting-row">
        <div className="setting-text">
          <p className="setting-label">{t('Recovery-Kit')}</p>
          <p className="setting-description">
            {t(
              'Der Code, den dein Tresor neben dem Master-Passwort braucht. Dieses Gerät zeigt ihn nach dem Master-Passwort noch einmal.',
            )}
          </p>
        </div>
        <div className="setting-control">
          <button onClick={() => setAskingKit(true)}>
            <Icon name="key" size={15} />
            {t('Anzeigen…')}
          </button>
        </div>
      </div>

      <div className="setting-row">
        <div className="setting-text">
          <p className="setting-label">{t('Trennen')}</p>
          <p className="setting-description">
            {t(
              'Dieses Gerät synchronisiert nicht mehr. Hosts und Tresor bleiben hier, das Master-Passwort allein öffnet ihn wieder.',
            )}
          </p>
        </div>
        <div className="setting-control">
          <button className="danger" onClick={() => setLeaving(true)}>
            {t('Trennen…')}
          </button>
        </div>
      </div>

      {adding && (
        <PasswordConfirm
          title={t('Gerät hinzufügen')}
          lead={t(
            'Das neue Gerät bekommt den Schlüssel zu deinem Tresor. Darum braucht es hier zuerst das Master-Passwort.',
          )}
          action={t('Code zeigen')}
          tone="normal"
          run={async (password) => setOffer(await syncOffer(password))}
          onCancel={() => setAdding(false)}
          onDone={() => setAdding(false)}
        />
      )}
      {offer && (
        <AddDevice
          offer={offer}
          onClose={(name) => {
            setOffer(null);
            if (name) {
              setJoined(name);
              loadDevices();
            }
            onChanged();
          }}
        />
      )}
      {revoking && (
        <PasswordConfirm
          title={t('{name} widerrufen?', { name: revoking.name })}
          lead={t(
            'Zum Widerrufen eines anderen Geräts braucht es das Master-Passwort – so kann ein gestohlenes Gerät dich nicht aussperren. Das Gerät bekommt danach nichts Neues mehr, aber was es schon hat, bleibt dort lesbar: Ändere Passwörter und Schlüssel, die darauf lagen, wenn du ihm nicht mehr traust.',
          )}
          action={t('Widerrufen')}
          run={(password) => syncRevoke(revoking.id, password)}
          onCancel={() => setRevoking(null)}
          onDone={() => {
            setRevoked(revoking.name);
            setRevoking(null);
            loadDevices();
          }}
        />
      )}
      {leaving && (
        <PasswordConfirm
          title={t('Vom Server trennen?')}
          lead={t(
            'Der Tresor wird wieder nur mit dem Master-Passwort verschlüsselt. Die anderen Geräte synchronisieren weiter.',
          )}
          action={t('Trennen')}
          run={(password) => syncDisconnect(password)}
          onCancel={() => setLeaving(false)}
          onDone={() => {
            setLeaving(false);
            onChanged();
          }}
        />
      )}
      {askingKit && (
        <PasswordConfirm
          title={t('Recovery-Kit anzeigen')}
          lead={t(
            'Der Code öffnet zusammen mit dem Master-Passwort deinen Tresor auf jedem Gerät. Zeig ihn niemandem und schreib ihn nur dorthin, wo er sicher ist.',
          )}
          action={t('Anzeigen')}
          tone="normal"
          run={async (password) => {
            const shown = await syncRecoveryCode(password);
            setKit(shown);
          }}
          onCancel={() => setAskingKit(false)}
          onDone={() => setAskingKit(false)}
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

/** The code to read out or paste, while this device waits for the other one. */
function AddDevice({ offer, onClose }: { offer: Offer; onClose: (joined: string | null) => void }) {
  useLanguage();
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [now, setNow] = useState(Date.now());
  const closed = useRef(false);
  const mounted = useRef(true);
  const waiting = useRef(false);

  useEffect(() => {
    mounted.current = true;
    // Once per code, however often React runs this: a second wait on the
    // same code would garble the handshake for the other device.
    if (!waiting.current) {
      waiting.current = true;
      void (async () => {
        try {
          const joined = await syncWaitForDevice();
          if (mounted.current && !closed.current) onClose(joined.name);
        } catch (e) {
          if (mounted.current && !closed.current) setError(failureText(asSyncFailure(e)));
        }
      })();
    }
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => {
      mounted.current = false;
      window.clearInterval(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const cancel = () => {
    closed.current = true;
    void syncCancelOffer().catch(() => undefined);
    onClose(null);
  };

  const left = Math.max(0, Math.round((offer.expiresMs - now) / 1000));
  const minutes = Math.floor(left / 60);
  const seconds = String(left % 60).padStart(2, '0');

  return (
    <Modal
      title={t('Gerät hinzufügen')}
      onCancel={cancel}
      footer={
        <>
          <span className="spacer" />
          <button data-secondary onClick={cancel}>
            {error ? t('Schließen') : t('Abbrechen')}
          </button>
        </>
      }
    >
      <NyuScene name="connecting" className="dialog-scene" />
      <p className="dialog-lead">
        {t('Auf dem neuen Gerät: Einstellungen → Sync → Mit einem Gerät koppeln.')}
      </p>
      <div className="sync-offer">
        <span className="setting-description">{t('Zum Abtippen')}</span>
        <code className="sync-kit-code">{offer.spoken}</code>
        <span className="setting-description">
          {t('Dazu die Adresse {server}', { server: offer.serverUrl })}
          {offer.tlsFingerprint && (
            <>
              {' · '}
              <code className="sync-fingerprint">{offer.tlsFingerprint}</code>
            </>
          )}
        </span>
      </div>
      <button
        onClick={() => {
          void copy(offer.pasteable);
          setCopied(true);
        }}
      >
        <Icon name={copied ? 'check' : 'copy'} size={15} />
        {copied ? t('Langen Code kopiert') : t('Langen Code zum Einfügen kopieren')}
      </button>
      {!error && (
        <p className="field-hint" role="status">
          {t('Warte auf das andere Gerät… noch {m}:{s}', { m: minutes, s: seconds })}
        </p>
      )}
      {error && (
        <p className="field-error" role="alert">
          {error}
        </p>
      )}
    </Modal>
  );
}

/** A question that needs the master password to go ahead. */
function PasswordConfirm({
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
