import { Fragment, useEffect, useState, type FormEvent, type ReactNode } from 'react';
import { t, useLanguage } from '../lib/i18n';
import type { HostRecord, ObservedCertificate, TypedLogin } from '../lib/session';
import { Icon } from './Icon';
import { Modal } from './Modal';

/*
 * The questions connecting can raise.
 *
 * Wording rule from docs/design.md, and not negotiable: dialogs about server
 * certificates are plain. No kaomoji, no Nyu, no jokes — a possible man in the
 * middle is the one moment the app must not sound like it is playing.
 */

/** `text` with its `{name}` placeholders replaced by elements. */
function withElements(text: string, elements: Record<string, ReactNode>): ReactNode[] {
  return text
    .split(/\{(\w+)\}/)
    .map((piece, index) =>
      index % 2 === 0 ? piece : <Fragment key={index}>{elements[piece]}</Fragment>,
    );
}

/** `DOMAIN\user` or `user@domain` typed into the user field splits into its parts. */
export function splitLogin(user: string, domain: string): { username: string; domain: string } {
  const trimmed = user.trim();
  const backslash = trimmed.indexOf('\\');
  if (backslash > 0 && !domain.trim()) {
    return { username: trimmed.slice(backslash + 1), domain: trimmed.slice(0, backslash) };
  }
  return { username: trimmed, domain: domain.trim() };
}

// ── Login ───────────────────────────────────────────────────────────────────

type LoginPromptProps = {
  host: HostRecord;
  /** For the gateway in front of the host rather than the host itself. */
  gateway?: boolean;
  username: string;
  domain: string;
  /** The previous attempt was rejected. */
  retry: boolean;
  /** Offer to keep the login for the host. */
  canSave: boolean;
  onSubmit: (login: TypedLogin, save: boolean) => void;
  onCancel: () => void;
};

export function LoginPrompt({
  host,
  gateway = false,
  username: initialUser,
  domain: initialDomain,
  retry,
  canSave,
  onSubmit,
  onCancel,
}: LoginPromptProps) {
  useLanguage();
  const [username, setUsername] = useState(initialUser);
  const [domain, setDomain] = useState(initialDomain);
  const [password, setPassword] = useState('');
  const [save, setSave] = useState(true);
  const [show, setShow] = useState(false);

  const submit = (event?: FormEvent) => {
    event?.preventDefault();
    const login = { ...splitLogin(username, domain), password };
    if (!login.username) return;
    // Do not keep the password in component state any longer than the submit.
    setPassword('');
    onSubmit(login, canSave && save);
  };

  const target = gateway ? (host.rdp.gateway?.address ?? host.address) : host.address;

  return (
    <Modal
      title={gateway ? t('Anmeldung am Gateway') : t('Anmeldung')}
      onCancel={onCancel}
      footer={
        <>
          <span className="spacer" />
          <button data-secondary onClick={onCancel}>
            {t('Abbrechen')}
          </button>
          <button className="primary" onClick={() => submit()}>
            {t('Verbinden')}
          </button>
        </>
      }
    >
      <form className="form" onSubmit={submit}>
        <p className="dialog-lead">
          {withElements(t('für {target}'), { target: <code>{target}</code> })}
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
              data-autofocus={initialUser ? undefined : true}
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
        <label className="field">
          <span>{t('Passwort')}</span>
          <span className="input-with-button">
            <input
              type={show ? 'text' : 'password'}
              value={password}
              onChange={(e) => setPassword(e.target.value)}
              autoComplete="off"
              aria-invalid={retry}
              data-autofocus={initialUser ? true : undefined}
            />
            <button
              type="button"
              className="icon-button"
              onClick={() => setShow((v) => !v)}
              aria-label={show ? t('Passwort verbergen') : t('Passwort anzeigen')}
            >
              <Icon name="eye" size={15} />
            </button>
          </span>
          {retry && (
            <em className="field-error">
              {gateway
                ? t('Das Gateway hat die Anmeldung abgelehnt.')
                : t('Der Server hat die Anmeldung abgelehnt.')}
            </em>
          )}
        </label>
        {canSave ? (
          <label className="check">
            <input type="checkbox" checked={save} onChange={(e) => setSave(e.target.checked)} />
            <span>
              <b>{t('Für diesen Host speichern')}</b>
              <small>
                {t(
                  'Das Passwort liegt dann verschlüsselt im Tresor; beim nächsten Mal verbindet UwURDP ohne zu fragen.',
                )}
              </small>
            </span>
          </label>
        ) : (
          <p className="field-hint">{t('Wird nur für diese Verbindung verwendet.')}</p>
        )}
        <button type="submit" hidden />
      </form>
    </Modal>
  );
}

// ── Certificates ────────────────────────────────────────────────────────────

/**
 * The SHA-1 thumbprint Windows shows for a certificate (certlm.msc, the
 * `Cert:` drive in PowerShell), so the one on the server can be compared at a
 * glance.
 */
function useThumbprint(derBase64: string): string | null {
  const [thumbprint, setThumbprint] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    try {
      const der = Uint8Array.from(atob(derBase64), (c) => c.charCodeAt(0));
      void crypto.subtle
        .digest('SHA-1', der)
        .then((hash) => {
          if (cancelled) return;
          setThumbprint(
            [...new Uint8Array(hash)]
              .map((b) => b.toString(16).padStart(2, '0'))
              .join('')
              .toUpperCase(),
          );
        })
        .catch(() => undefined);
    } catch {
      setThumbprint(null);
    }
    return () => {
      cancelled = true;
    };
  }, [derBase64]);
  return thumbprint;
}

function CertificateFacts({ observed }: { observed: ObservedCertificate }) {
  useLanguage();
  const thumbprint = useThumbprint(observed.derBase64);
  return (
    <dl className="key-compare">
      <dt>{t('Ausgestellt für')}</dt>
      <dd>
        <code>{observed.subject || '—'}</code>
      </dd>
      <dt>{t('Ausgestellt von')}</dt>
      <dd>
        <code>{observed.issuer || '—'}</code>
      </dd>
      <dt>{t('Gültig')}</dt>
      <dd>
        <code>
          {observed.notBefore} – {observed.notAfter}
        </code>
      </dd>
      {thumbprint && (
        <>
          <dt>{t('Thumbprint')}</dt>
          <dd>
            <code className="fingerprint">{thumbprint}</code>
          </dd>
        </>
      )}
      <dt>{t('Fingerprint')}</dt>
      <dd>
        <code className="fingerprint">{observed.fingerprint}</code>
      </dd>
    </dl>
  );
}

type TrustProps = {
  host: HostRecord;
  observed: ObservedCertificate;
  onTrust: () => void;
  onCancel: () => void;
};

export function TrustCertificate({ host, observed, onTrust, onCancel }: TrustProps) {
  useLanguage();
  return (
    <Modal
      title={t('Unbekanntes Zertifikat')}
      onCancel={onCancel}
      footer={
        <>
          <span className="spacer" />
          <button data-secondary onClick={onCancel}>
            {t('Abbrechen')}
          </button>
          <button className="primary" data-secondary onClick={onTrust}>
            {t('Vertrauen und verbinden')}
          </button>
        </>
      }
    >
      <p className="dialog-lead">
        {withElements(
          t('Erste Verbindung zu {address}. Bisher wurde nichts gesendet — auch kein Passwort.'),
          {
            address: (
              <code>
                {host.address}:{host.port}
              </code>
            ),
          },
        )}
      </p>
      <CertificateFacts observed={observed} />
      <p className="field-hint">
        {withElements(
          t(
            'Windows-Server nutzen meist ein selbst ausgestelltes Zertifikat. Vergleiche den Thumbprint mit dem auf dem Server, etwa per {command}. Stimmt er, merkt sich UwURDP das Zertifikat und fragt beim nächsten Mal nicht mehr.',
          ),
          {
            command: <code>Get-ChildItem &apos;Cert:\LocalMachine\Remote Desktop&apos;</code>,
          },
        )}
      </p>
    </Modal>
  );
}

type ChangedProps = {
  host: HostRecord;
  expected: string;
  observed: ObservedCertificate;
  onAccept: () => void;
  onReject: () => void;
};

/**
 * Accept or reject, with two buttons. Rejecting has the focus and is where
 * Enter and Escape lead; accepting needs a deliberate click, and the warning
 * above it says plainly what it can mean.
 */
export function CertificateChanged({ host, expected, observed, onAccept, onReject }: ChangedProps) {
  useLanguage();
  return (
    <Modal
      title={t('Das Zertifikat hat sich geändert')}
      tone="warning"
      onCancel={onReject}
      footer={
        <>
          <button className="danger" data-secondary onClick={onAccept}>
            {t('Neues Zertifikat akzeptieren')}
          </button>
          <span className="spacer" />
          <button className="primary" data-autofocus onClick={onReject}>
            {t('Ablehnen')}
          </button>
        </>
      }
    >
      <p className="dialog-lead">
        {withElements(
          t(
            '{address} zeigt ein anderes Zertifikat als beim letzten Mal. Das kann ein erneuertes Zertifikat sein — Windows tauscht selbst ausgestellte gelegentlich aus — oder jemand, der sich zwischen dich und den Server schaltet.',
          ),
          {
            address: (
              <code>
                {host.address}:{host.port}
              </code>
            ),
          },
        )}
      </p>
      <p className="dialog-lead">
        <strong>{t('Die Verbindung wurde abgebrochen. Es wurde nichts gesendet.')}</strong>
      </p>
      <dl className="key-compare">
        <dt>{t('Bisher vertraut')}</dt>
        <dd>
          <code className="fingerprint">{expected}</code>
        </dd>
      </dl>
      <CertificateFacts observed={observed} />
      <p className="field-hint">
        {t(
          'Akzeptiere nur, wenn du weißt, dass der Server ein neues Zertifikat bekommen hat. Im Zweifel: ablehnen und nachfragen.',
        )}
      </p>
    </Modal>
  );
}
