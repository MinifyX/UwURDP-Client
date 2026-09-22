import { useState, type ReactNode } from 'react';
import { t, useLanguage } from '../lib/i18n';
import { Modal } from './Modal';

/**
 * Closing a dialog that holds unsaved input asks first. The dialog's close
 * button, "Abbrechen" and Escape call `request`; `dialog` is the question,
 * rendered next to the dialog while it is open. With nothing unsaved,
 * `request` just closes.
 */
export function useCloseGuard(
  unsaved: boolean,
  close: () => void,
  loss = t('Was du bisher eingegeben hast, geht dabei verloren.'),
): { request: () => void; dialog: ReactNode } {
  useLanguage();
  const [asking, setAsking] = useState(false);
  const request = () => (unsaved ? setAsking(true) : close());
  const dialog = asking ? (
    <Modal
      title={t('Wirklich schließen?')}
      onCancel={() => setAsking(false)}
      footer={
        <>
          <span className="spacer" />
          <button
            className="danger"
            data-secondary
            onClick={() => {
              setAsking(false);
              close();
            }}
          >
            {t('Schließen')}
          </button>
          <button className="primary" data-autofocus onClick={() => setAsking(false)}>
            {t('Weiter bearbeiten')}
          </button>
        </>
      }
    >
      <p className="dialog-lead">{loss}</p>
    </Modal>
  ) : null;
  return { request, dialog };
}
