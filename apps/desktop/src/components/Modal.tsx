import { useEffect, useId, useRef, type ReactNode } from 'react';
import { createPortal } from 'react-dom';

type ModalProps = {
  title: string;
  /** Security warnings get their own look, so they never blend in with routine dialogs. */
  tone?: 'default' | 'warning';
  /** Settings need room for a section list next to the content. */
  size?: 'default' | 'wide';
  onCancel: () => void;
  children: ReactNode;
  footer?: ReactNode;
};

const FOCUSABLE = 'input, button, textarea, select, [href], [tabindex]:not([tabindex="-1"])';

/**
 * Open dialogs, innermost last. A dialog can open another (the host form opens
 * the vault), and only the one on top may react to Escape and Tab.
 */
const stack: HTMLElement[] = [];

/**
 * A dialog. Escape cancels, a click beside it doesn't; focus moves into the
 * dialog on open, stays inside it while it is open, and goes back where it was
 * on close.
 *
 * Focus always lands somewhere inside. When nothing should be focused — the
 * host key dialog deliberately gives neither button default focus, so Enter
 * cannot trust a key by accident — the dialog itself takes it. Leaving focus
 * behind let Enter activate whatever was focused in the background: in the
 * end-to-end test that was the host row, and it started a second connection.
 */
export function Modal({
  title,
  tone = 'default',
  size = 'default',
  onCancel,
  children,
  footer,
}: ModalProps) {
  const dialogRef = useRef<HTMLDivElement>(null);
  // Escape calls the latest onCancel, not the one from when the dialog
  // opened: a form that asks before closing only knows once something changed.
  const cancelRef = useRef(onCancel);
  cancelRef.current = onCancel;
  // Dialogs stack (the host form opens the vault): each needs its own title id.
  const titleId = useId();

  useEffect(() => {
    const dialog = dialogRef.current;
    if (!dialog) return;
    const previous = document.activeElement as HTMLElement | null;

    // An explicit [data-autofocus] wins — warnings point it at the safe choice.
    // Otherwise the first field, or the first button not marked secondary.
    // Buttons that act on something risky carry data-secondary, so Enter can
    // never trigger them by accident.
    const first =
      dialog.querySelector<HTMLElement>('[data-autofocus]') ??
      dialog.querySelector<HTMLElement>('input, button:not([data-secondary]), textarea, select') ??
      dialog;
    first.focus();
    stack.push(dialog);

    const onKey = (event: KeyboardEvent) => {
      if (stack[stack.length - 1] !== dialog) return;
      if (event.key === 'Escape') {
        event.preventDefault();
        cancelRef.current();
        return;
      }
      if (event.key !== 'Tab') return;

      // Keep Tab inside the dialog.
      const focusable = [...dialog.querySelectorAll<HTMLElement>(FOCUSABLE)].filter(
        (el) => !el.hasAttribute('disabled') && !el.hidden,
      );
      if (focusable.length === 0) {
        event.preventDefault();
        return;
      }
      const firstEl = focusable[0]!;
      const lastEl = focusable[focusable.length - 1]!;
      const current = document.activeElement;
      if (event.shiftKey && (current === firstEl || current === dialog)) {
        event.preventDefault();
        lastEl.focus();
      } else if (!event.shiftKey && current === lastEl) {
        event.preventDefault();
        firstEl.focus();
      }
    };

    window.addEventListener('keydown', onKey);
    return () => {
      window.removeEventListener('keydown', onKey);
      stack.splice(stack.indexOf(dialog), 1);
      previous?.focus();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Straight into <body>: a dialog opened from inside another one (the export
  // from Settings) otherwise lives in the outer dialog's scroll box, which
  // moves it about when a field inside gets focus.
  return createPortal(
    // A click beside the dialog does nothing: closing by accident threw away
    // whatever was typed into it. Escape and the dialog's own buttons close.
    <div className="modal-backdrop">
      <div
        ref={dialogRef}
        className="modal"
        data-tone={tone}
        data-size={size}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
      >
        <h2 id={titleId} className="modal-title">
          {title}
        </h2>
        <div className="modal-body">{children}</div>
        {footer && <div className="modal-footer">{footer}</div>}
      </div>
    </div>,
    document.body,
  );
}
