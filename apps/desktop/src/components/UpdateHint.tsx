import { useState } from 'react';
import { language, t, useLanguage } from '../lib/i18n';
import type { UpdateInfo } from '../lib/session';
import { Nyu } from './nyu/Nyu';

/** Release notes are JSON with `de` and `en` when written for UwURDP, otherwise plain text. */
export function notesFor(notes: string | null | undefined): string {
  if (!notes) return '';
  try {
    const parsed = JSON.parse(notes) as Record<string, unknown>;
    const text = language() === 'en' ? (parsed.en ?? parsed.de) : (parsed.de ?? parsed.en);
    if (typeof text === 'string') return text;
  } catch {
    // Plain text notes.
  }
  return notes;
}

type Props = {
  update: UpdateInfo;
  /** Remote sessions a restart would cut off. */
  openConnections: number;
  onLater: () => void;
  onRestart: () => Promise<void>;
};

/** Nyu's quiet note that a new version is downloaded and ready. */
export function UpdateHint({ update, openConnections, onLater, onRestart }: Props) {
  useLanguage();
  const [showNotes, setShowNotes] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const notes = notesFor(update.notes);

  return (
    <aside className="update-hint" aria-live="polite">
      <div className="update-hint-head">
        <Nyu size={40} mood="sparkle" title="Nyu" />
        <div>
          <p className="update-hint-title">{t('Ein Update ist bereit ✧')}</p>
          <p className="update-hint-meta">
            {t('Version {version}', { version: update.version })}
            {notes && (
              <>
                {' · '}
                <button
                  className="link-button"
                  onClick={() => setShowNotes(!showNotes)}
                  aria-expanded={showNotes}
                >
                  {t('Was ist neu?')}
                </button>
              </>
            )}
          </p>
        </div>
      </div>
      {showNotes && <p className="update-hint-notes">{notes}</p>}
      {openConnections > 0 && (
        <p className="update-hint-warning">
          {openConnections === 1
            ? t('Eine Verbindung ist noch offen und wird beim Neustart getrennt.')
            : t('{count} Verbindungen sind noch offen und werden beim Neustart getrennt.', {
                count: openConnections,
              })}
        </p>
      )}
      {error && (
        <p className="update-hint-warning" role="alert">
          {error}
        </p>
      )}
      <div className="update-hint-actions">
        <button className="quiet" onClick={onLater}>
          {t('Später')}
        </button>
        <button
          className="primary"
          disabled={restarting}
          onClick={async () => {
            setRestarting(true);
            setError(null);
            try {
              await onRestart();
            } catch (e) {
              setError(String(e));
              setRestarting(false);
            }
          }}
        >
          {restarting ? t('Startet neu …') : t('Jetzt neu starten')}
        </button>
      </div>
    </aside>
  );
}
