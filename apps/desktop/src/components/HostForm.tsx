import { useRef, useState, type FormEvent, type ReactNode } from 'react';
import { N_, t } from '../lib/i18n';
import {
  DEFAULT_RDP,
  deleteHost,
  loginLabel,
  saveHost,
  type AudioMode,
  type DisplayMode,
  type GroupRecord,
  type HostRecord,
  type PasswordChange,
  type RdpSettings,
  type SaveFailure,
  type Workspace,
} from '../lib/session';
import { useCloseGuard } from './CloseGuard';
import { splitLogin } from './ConnectDialogs';
import { useSettings, workspaceName } from '../lib/settings';
import { Icon } from './Icon';
import { Modal } from './Modal';
import { VaultDialog } from './VaultDialog';

type Props = {
  /** The host to edit, or nothing for a new one. */
  host: HostRecord | null;
  /** Where a new host goes. */
  workspace?: Workspace;
  group?: string | null;
  groups: GroupRecord[];
  onSaved: (host: HostRecord) => void;
  onDeleted: (id: string) => void;
  onCancel: () => void;
};

/** What the store's validation codes mean, next to the field they belong to. */
const PROBLEMS: Record<string, Record<string, string>> = {
  address: {
    required: N_('Adresse fehlt'),
    whitespace: N_('Die Adresse darf keine Leerzeichen enthalten'),
  },
  username: {
    required: N_('Zum Passwort gehört ein Benutzer'),
    control: N_('Ungültiger Benutzername'),
    'too-long': N_('Der Benutzername ist zu lang'),
  },
  domain: {
    whitespace: N_('Die Domäne darf keine Leerzeichen enthalten'),
    'too-long': N_('Die Domäne ist zu lang'),
  },
  gatewayUsername: { required: N_('Zum Passwort gehört ein Benutzer') },
  gatewayDomain: { whitespace: N_('Die Domäne darf keine Leerzeichen enthalten') },
  gatewayAddress: { whitespace: N_('Die Adresse darf keine Leerzeichen enthalten') },
  port: { 'out-of-range': N_('Port zwischen 1 und 65535') },
  password: { required: N_('Das Passwort ist leer') },
  comment: { 'too-long': N_('Der Kommentar ist zu lang') },
  groupPath: {
    'too-long': N_('Der Gruppenname ist zu lang'),
    control: N_('Ungültiger Gruppenname'),
  },
};

type Field =
  | 'address'
  | 'port'
  | 'username'
  | 'domain'
  | 'password'
  | 'groupPath'
  | 'comment'
  | 'gatewayAddress'
  | 'gatewayUsername'
  | 'gatewayDomain'
  | 'form';
type Errors = Partial<Record<Field, string>>;

/** Common desktop sizes for "fixed", largest last. */
const SIZES: [number, number][] = [
  [1024, 768],
  [1280, 720],
  [1280, 1024],
  [1366, 768],
  [1440, 900],
  [1600, 900],
  [1920, 1080],
  [1920, 1200],
  [2560, 1440],
  [3840, 2160],
];

/** The password field of one login: keep, change, forget. */
function PasswordField({
  stored,
  value,
  forget,
  show,
  error,
  hint,
  onValue,
  onForget,
  onShow,
}: {
  stored: boolean;
  value: string | null;
  forget: boolean;
  show: boolean;
  error?: string;
  hint: string;
  onValue: (value: string | null) => void;
  onForget: (forget: boolean) => void;
  onShow: () => void;
}) {
  if (stored && !forget && value === null) {
    return (
      <div className="stored-secret">
        <Icon name="lock" size={15} />
        <span>{t('Das Passwort ist im Tresor gespeichert.')}</span>
        <span className="spacer" />
        <button type="button" className="quiet" onClick={() => onValue('')}>
          {t('Ändern')}
        </button>
        <button type="button" className="quiet" onClick={() => onForget(true)}>
          {t('Entfernen')}
        </button>
      </div>
    );
  }
  if (forget) {
    return (
      <div className="stored-secret" data-forgotten>
        <Icon name="unlock" size={15} />
        <span>{t('Das gespeicherte Passwort wird beim Speichern entfernt.')}</span>
        <span className="spacer" />
        <button type="button" className="quiet" onClick={() => onForget(false)}>
          {t('Rückgängig')}
        </button>
      </div>
    );
  }
  return (
    <label className="field">
      <span>{t('Passwort')}</span>
      <span className="input-with-button">
        <input
          type={show ? 'text' : 'password'}
          value={value ?? ''}
          onChange={(event) => onValue(event.target.value)}
          placeholder={t('leer lassen: beim Verbinden fragen')}
          autoComplete="new-password"
          aria-invalid={Boolean(error)}
        />
        <button
          type="button"
          className="icon-button"
          onClick={onShow}
          aria-label={show ? t('Passwort verbergen') : t('Passwort anzeigen')}
        >
          <Icon name="eye" size={15} />
        </button>
      </span>
      {error ? <em className="field-error">{t(error)}</em> : <em className="field-hint">{hint}</em>}
    </label>
  );
}

function Section({
  title,
  open,
  children,
}: {
  title: string;
  open?: boolean;
  children: ReactNode;
}) {
  return (
    <details className="form-section" open={open}>
      <summary>
        <Icon name="chevron" size={12} className="group-chevron" />
        {title}
      </summary>
      <div className="form-section-body">{children}</div>
    </details>
  );
}

function Check({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string;
  hint?: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="check">
      <input type="checkbox" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span>
        <b>{label}</b>
        {hint && <small>{hint}</small>}
      </span>
    </label>
  );
}

export function HostForm({ host, workspace, group, groups, onSaved, onDeleted, onCancel }: Props) {
  const settings = useSettings();
  const [name, setName] = useState(host?.name ?? '');
  const [address, setAddress] = useState(host?.address ?? '');
  const [port, setPort] = useState(String(host?.port ?? 3389));
  const [username, setUsername] = useState(host?.username ?? '');
  const [domain, setDomain] = useState(host?.domain ?? '');
  const [space, setSpace] = useState<Workspace>(host?.workspace ?? workspace ?? 'private');
  const [groupPath, setGroupPath] = useState(host?.groupPath ?? group ?? '');
  /** The password field: `null` keeps what is stored. */
  const [password, setPassword] = useState<string | null>(host?.hasPassword ? null : '');
  const [forget, setForget] = useState(false);
  const [showPassword, setShowPassword] = useState(false);
  const [rdp, setRdp] = useState<RdpSettings>({ ...DEFAULT_RDP, ...(host?.rdp ?? {}) });
  const [comment, setComment] = useState(host?.comment ?? '');
  const [gatewayUser, setGatewayUser] = useState(host?.gatewayUsername ?? '');
  const [gatewayDomain, setGatewayDomain] = useState(host?.gatewayDomain ?? '');
  const [gatewayPassword, setGatewayPassword] = useState<string | null>(
    host?.hasGatewayPassword ? null : '',
  );
  const [gatewayForget, setGatewayForget] = useState(false);
  const [showGatewayPassword, setShowGatewayPassword] = useState(false);
  const [errors, setErrors] = useState<Errors>({});
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [vault, setVault] = useState(false);

  // What the form holds, compared with what it opened with: closing a form
  // that changed asks first.
  const snapshot = JSON.stringify([
    name,
    address,
    port,
    username,
    domain,
    space,
    groupPath,
    password ?? null,
    forget,
    rdp,
    comment,
    gatewayUser,
    gatewayDomain,
    gatewayPassword ?? null,
    gatewayForget,
  ]);
  const opened = useRef(snapshot);
  const guard = useCloseGuard(
    snapshot !== opened.current,
    onCancel,
    host
      ? t('Deine Änderungen an diesem Host gehen dabei verloren.')
      : t('Der neue Host ist noch nicht gespeichert und geht dabei verloren.'),
  );

  /** Editing a field clears its error: a stale "Adresse fehlt" under a filled-in address is noise. */
  const clear = (field: Field) => {
    if (errors[field] || errors.form)
      setErrors((current) => ({ ...current, [field]: undefined, form: undefined }));
  };
  const edit =
    (field: Field, set: (value: string) => void) => (event: { target: { value: string } }) => {
      set(event.target.value);
      clear(field);
    };
  const patch = (change: Partial<RdpSettings>) => setRdp((current) => ({ ...current, ...change }));

  const change = (value: string | null, forgotten: boolean): PasswordChange => {
    if (forgotten) return { kind: 'forget' };
    if (value === null || value === '') return { kind: 'keep' };
    return { kind: 'set', value };
  };

  const submit = async (event?: FormEvent) => {
    event?.preventDefault();
    const portNumber = Number(port);
    if (!Number.isInteger(portNumber) || portNumber < 1 || portNumber > 65535) {
      setErrors({ port: PROBLEMS.port?.['out-of-range'] });
      return;
    }
    const login = splitLogin(username, domain);
    const gatewayLogin = splitLogin(gatewayUser, gatewayDomain);
    setBusy(true);
    setErrors({});
    try {
      const saved = await saveHost({
        id: host?.id ?? null,
        name,
        address,
        port: portNumber,
        username: login.username,
        domain: login.domain,
        groupPath: groupPath || null,
        workspace: space,
        password: change(password, forget),
        rdp: { ...rdp, gateway: rdp.gateway?.address.trim() ? rdp.gateway : null },
        comment,
        gatewayUsername: rdp.gateway && !rdp.gateway.useHostLogin ? gatewayLogin.username : '',
        gatewayDomain: rdp.gateway && !rdp.gateway.useHostLogin ? gatewayLogin.domain : '',
        gatewayPassword:
          rdp.gateway && !rdp.gateway.useHostLogin
            ? change(gatewayPassword, gatewayForget)
            : host?.hasGatewayPassword
              ? { kind: 'forget' }
              : { kind: 'keep' },
      });
      setPassword(null);
      onSaved(saved);
    } catch (raw) {
      const failure = raw as SaveFailure;
      if (failure?.kind === 'vault-locked') {
        setVault(true);
      } else if (failure?.kind === 'invalid') {
        setErrors({
          [failure.field]: PROBLEMS[failure.field]?.[failure.problem] ?? failure.problem,
        });
      } else {
        setErrors({ form: failure?.kind === 'error' ? failure.message : String(raw) });
      }
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!host) return;
    if (!confirmDelete) {
      setConfirmDelete(true);
      return;
    }
    setBusy(true);
    try {
      await deleteHost(host.id);
      onDeleted(host.id);
    } catch (raw) {
      setErrors({ form: String(raw) });
      setBusy(false);
    }
  };

  const groupNames = [...new Set(groups.filter((g) => g.workspace === space).map((g) => g.name))];
  const groupLogin = groups.find(
    (g) => g.workspace === space && g.name === groupPath.trim() && g.username,
  );
  const sizeValue = `${rdp.width}x${rdp.height}`;
  const knownSize = SIZES.some(([w, h]) => `${w}x${h}` === sizeValue);

  return (
    <>
      <Modal
        title={host ? t('{name} bearbeiten', { name: host.name }) : t('Neuer Host')}
        onCancel={guard.request}
        footer={
          <>
            {host && (
              <button className="danger" data-secondary onClick={remove} disabled={busy}>
                {confirmDelete ? t('Wirklich löschen') : t('Löschen')}
              </button>
            )}
            <span className="spacer" />
            <button data-secondary onClick={guard.request} disabled={busy}>
              {t('Abbrechen')}
            </button>
            <button className="primary" onClick={() => void submit()} disabled={busy}>
              {t('Speichern')}
            </button>
          </>
        }
      >
        <form className="form" onSubmit={submit}>
          <div className="form-row">
            <label className="field grow">
              <span>{t('Adresse')}</span>
              <input
                value={address}
                onChange={edit('address', setAddress)}
                placeholder={t('10.0.0.12 oder dc-1.firma.local')}
                aria-invalid={Boolean(errors.address)}
                autoComplete="off"
                spellCheck={false}
              />
              {errors.address && <em className="field-error">{t(errors.address)}</em>}
            </label>
            <label className="field port">
              <span>{t('Port')}</span>
              <input
                value={port}
                onChange={edit('port', setPort)}
                inputMode="numeric"
                aria-invalid={Boolean(errors.port)}
              />
              {errors.port && <em className="field-error">{t(errors.port)}</em>}
            </label>
          </div>

          <label className="field">
            <span>{t('Name')}</span>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={address || t('wie die Adresse')}
              autoComplete="off"
            />
          </label>

          <div className="form-row">
            {settings.workspaces && (
              <fieldset className="field">
                <span>{t('Bereich')}</span>
                <div className="segmented" role="radiogroup" aria-label={t('Bereich')}>
                  {(['private', 'business'] as const).map((id) => (
                    <button
                      key={id}
                      type="button"
                      role="radio"
                      aria-checked={space === id}
                      onClick={() => setSpace(id)}
                    >
                      {workspaceName(id, settings)}
                    </button>
                  ))}
                </div>
              </fieldset>
            )}
            <label className="field grow">
              <span>{t('Gruppe')}</span>
              <input
                value={groupPath}
                onChange={edit('groupPath', setGroupPath)}
                placeholder={t('optional, z. B. Domain Controller')}
                autoComplete="off"
                list="host-form-groups"
                aria-invalid={Boolean(errors.groupPath)}
              />
              <datalist id="host-form-groups">
                {groupNames.map((groupName) => (
                  <option key={groupName} value={groupName} />
                ))}
              </datalist>
              {errors.groupPath && <em className="field-error">{t(errors.groupPath)}</em>}
            </label>
          </div>

          <Section title={t('Anmeldung')} open>
            <div className="form-row">
              <label className="field grow">
                <span>{t('Benutzer')}</span>
                <input
                  value={username}
                  onChange={edit('username', setUsername)}
                  placeholder={
                    groupLogin
                      ? loginLabel(groupLogin.username, groupLogin.domain)
                      : t('leer lassen: beim Verbinden fragen')
                  }
                  aria-invalid={Boolean(errors.username)}
                  autoComplete="off"
                  spellCheck={false}
                />
                {errors.username && <em className="field-error">{t(errors.username)}</em>}
              </label>
              <label className="field grow">
                <span>{t('Domäne')}</span>
                <input
                  value={domain}
                  onChange={edit('domain', setDomain)}
                  placeholder={t('optional')}
                  aria-invalid={Boolean(errors.domain)}
                  autoComplete="off"
                  spellCheck={false}
                />
                {errors.domain && <em className="field-error">{t(errors.domain)}</em>}
              </label>
            </div>
            {username.trim() ? (
              <PasswordField
                stored={Boolean(host?.hasPassword && host.username)}
                value={password}
                forget={forget}
                show={showPassword}
                error={errors.password}
                hint={t(
                  'Gespeichert wird es verschlüsselt im Tresor – UwURDP verbindet dann ohne zu fragen.',
                )}
                onValue={(value) => {
                  setPassword(value);
                  clear('password');
                }}
                onForget={setForget}
                onShow={() => setShowPassword((v) => !v)}
              />
            ) : (
              <p className="field-hint login-inherit">
                <Icon name="user" size={14} />
                {groupLogin
                  ? t(
                      'Ohne eigenen Benutzer meldet sich der Host mit dem Login der Gruppe {group} an: {login}.',
                      {
                        group: groupLogin.name,
                        login: loginLabel(groupLogin.username, groupLogin.domain),
                      },
                    )
                  : t(
                      'Ohne Benutzer fragt UwURDP beim Verbinden – oder nimmt den Login der Gruppe, sobald sie einen hat.',
                    )}
              </p>
            )}
          </Section>

          <Section title={t('Anzeige')}>
            <fieldset className="field">
              <span>{t('Größe des Desktops')}</span>
              <div className="segmented" role="radiogroup" aria-label={t('Größe des Desktops')}>
                {(
                  [
                    ['fit', t('An den Tab anpassen')],
                    ['fixed', t('Feste Größe')],
                    ['fullscreen', t('Vollbild')],
                  ] as [DisplayMode, string][]
                ).map(([value, label]) => (
                  <button
                    key={value}
                    type="button"
                    role="radio"
                    aria-checked={rdp.display === value}
                    onClick={() => patch({ display: value })}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </fieldset>
            {rdp.display === 'fixed' && (
              <label className="field">
                <span>{t('Auflösung')}</span>
                <select
                  className="select"
                  value={knownSize ? sizeValue : ''}
                  onChange={(e) => {
                    const [w, h] = e.target.value.split('x').map(Number);
                    if (w && h) patch({ width: w, height: h });
                  }}
                >
                  {!knownSize && <option value="">{`${rdp.width} × ${rdp.height}`}</option>}
                  {SIZES.map(([w, h]) => (
                    <option key={`${w}x${h}`} value={`${w}x${h}`}>
                      {w} × {h}
                    </option>
                  ))}
                </select>
              </label>
            )}
            <Check
              label={t('Einpassen statt scrollen')}
              hint={t(
                'Ist der Desktop größer als der Tab, wird er verkleinert angezeigt (Smart Sizing).',
              )}
              checked={rdp.smartSizing}
              onChange={(smartSizing) => patch({ smartSizing })}
            />
            <label className="field">
              <span>{t('Farbtiefe')}</span>
              <select
                className="select"
                value={rdp.colorDepth}
                onChange={(e) => patch({ colorDepth: Number(e.target.value) })}
              >
                <option value={32}>{t('32 Bit (höchste Qualität)')}</option>
                <option value={24}>{t('24 Bit')}</option>
                <option value={16}>{t('16 Bit (langsame Leitungen)')}</option>
                <option value={15}>{t('15 Bit')}</option>
              </select>
            </label>
            <Check
              label={t('Hintergrundbild zeigen')}
              hint={t('Aus spart Bandbreite.')}
              checked={rdp.wallpaper}
              onChange={(wallpaper) => patch({ wallpaper })}
            />
          </Section>

          <Section title={t('Lokale Ressourcen')}>
            <fieldset className="field">
              <span>{t('Ton')}</span>
              <div className="segmented" role="radiogroup" aria-label={t('Ton')}>
                {(
                  [
                    ['local', t('Hier abspielen')],
                    ['remote', t('Auf dem Server')],
                    ['off', t('Aus')],
                  ] as [AudioMode, string][]
                ).map(([value, label]) => (
                  <button
                    key={value}
                    type="button"
                    role="radio"
                    aria-checked={rdp.audio === value}
                    onClick={() => patch({ audio: value })}
                  >
                    {label}
                  </button>
                ))}
              </div>
            </fieldset>
            <Check
              label={t('Zwischenablage teilen')}
              hint={t('Text kopieren und einfügen, in beide Richtungen.')}
              checked={rdp.clipboard}
              onChange={(clipboard) => patch({ clipboard })}
            />
          </Section>

          <Section title={t('Erweitert')}>
            <Check
              label={t('Konsolensitzung (Admin)')}
              hint={t(
                'Wie mstsc /admin. Wird gespeichert, wirkt aber erst in einer späteren Version – die RDP-Engine kann es noch nicht anfordern.',
              )}
              checked={rdp.admin}
              onChange={(admin) => patch({ admin })}
            />
            <Check
              label={t('Anmeldung auf Netzwerkebene (NLA)')}
              hint={t('Nur für sehr alte Server ausschalten.')}
              checked={rdp.nla}
              onChange={(nla) => patch({ nla })}
            />
            <Check
              label={t('Über ein Remotedesktopgateway verbinden')}
              hint={t(
                'Noch nicht unterstützt: wird gespeichert (auch aus RDCMan übernommen), verbinden über ein Gateway kommt in einer späteren Version.',
              )}
              checked={Boolean(rdp.gateway)}
              onChange={(on) =>
                patch({
                  gateway: on
                    ? { address: '', port: 443, useHostLogin: true, bypassLocal: false }
                    : null,
                })
              }
            />
            {rdp.gateway && (
              <div className="gateway-block">
                <div className="form-row">
                  <label className="field grow">
                    <span>{t('Gateway')}</span>
                    <input
                      value={rdp.gateway.address}
                      onChange={(e) => {
                        patch({ gateway: { ...rdp.gateway!, address: e.target.value } });
                        clear('gatewayAddress');
                      }}
                      placeholder="gateway.firma.de"
                      autoComplete="off"
                      spellCheck={false}
                      aria-invalid={Boolean(errors.gatewayAddress)}
                    />
                    {errors.gatewayAddress && (
                      <em className="field-error">{t(errors.gatewayAddress)}</em>
                    )}
                  </label>
                  <label className="field port">
                    <span>{t('Port')}</span>
                    <input
                      value={String(rdp.gateway.port)}
                      inputMode="numeric"
                      onChange={(e) =>
                        patch({
                          gateway: { ...rdp.gateway!, port: Number(e.target.value) || 443 },
                        })
                      }
                    />
                  </label>
                </div>
                <Check
                  label={t('Mit der Anmeldung des Hosts am Gateway anmelden')}
                  checked={rdp.gateway.useHostLogin}
                  onChange={(useHostLogin) => patch({ gateway: { ...rdp.gateway!, useHostLogin } })}
                />
                {!rdp.gateway.useHostLogin && (
                  <>
                    <div className="form-row">
                      <label className="field grow">
                        <span>{t('Benutzer am Gateway')}</span>
                        <input
                          value={gatewayUser}
                          onChange={edit('gatewayUsername', setGatewayUser)}
                          placeholder={t('leer lassen: beim Verbinden fragen')}
                          autoComplete="off"
                          spellCheck={false}
                          aria-invalid={Boolean(errors.gatewayUsername)}
                        />
                        {errors.gatewayUsername && (
                          <em className="field-error">{t(errors.gatewayUsername)}</em>
                        )}
                      </label>
                      <label className="field grow">
                        <span>{t('Domäne')}</span>
                        <input
                          value={gatewayDomain}
                          onChange={edit('gatewayDomain', setGatewayDomain)}
                          placeholder={t('optional')}
                          autoComplete="off"
                          spellCheck={false}
                        />
                      </label>
                    </div>
                    {gatewayUser.trim() && (
                      <PasswordField
                        stored={Boolean(host?.hasGatewayPassword)}
                        value={gatewayPassword}
                        forget={gatewayForget}
                        show={showGatewayPassword}
                        hint={t('Verschlüsselt im Tresor, wie das Passwort des Hosts.')}
                        onValue={setGatewayPassword}
                        onForget={setGatewayForget}
                        onShow={() => setShowGatewayPassword((v) => !v)}
                      />
                    )}
                  </>
                )}
                <Check
                  label={t('Gateway im lokalen Netz umgehen')}
                  checked={rdp.gateway.bypassLocal}
                  onChange={(bypassLocal) => patch({ gateway: { ...rdp.gateway!, bypassLocal } })}
                />
              </div>
            )}
          </Section>

          <label className="field">
            <span>{t('Kommentar')}</span>
            <textarea
              value={comment}
              onChange={edit('comment', setComment)}
              rows={2}
              placeholder={t('optional, z. B. wofür der Server da ist')}
              aria-invalid={Boolean(errors.comment)}
            />
            {errors.comment && <em className="field-error">{t(errors.comment)}</em>}
          </label>

          {errors.form && <p className="form-error">{errors.form}</p>}
          {/* Enter in any field submits. */}
          <button type="submit" hidden />
        </form>
      </Modal>

      {guard.dialog}
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
