/**
 * Dragging inside the app, with pointer events.
 *
 * Not HTML5 drag and drop: on Windows, Tauri's own file-drop handling (which
 * the file browser needs for files dragged in from Explorer) swallows the
 * webview's HTML5 drag events. Pointer events always arrive, work the same for
 * mouse, pen and touch, and let the ghost look like the app.
 *
 * A drag starts after the pointer moved a few pixels, so a click stays a
 * click. Drop targets are plain elements with `data-drop` and whatever else
 * the drop handler needs in their dataset; the element under the pointer gets
 * `data-drop-over` (`before`, `after` or `inside`) while it would take the drop.
 */

export type DropEdge = 'before' | 'after' | 'inside';

export type DropTarget = {
  element: HTMLElement;
  data: DOMStringMap;
  edge: DropEdge;
};

type DragOptions = {
  /** Text for the ghost that follows the pointer. */
  label: string;
  /** Whether this element takes the drop, and where. `null` refuses it. */
  accept: (element: HTMLElement, pointerY: number) => DropEdge | null;
  onDrop: (target: DropTarget) => void;
  onStart?: () => void;
  onEnd?: () => void;
};

const THRESHOLD = 5;

/** Starts watching a pointer that went down; becomes a drag once it moves. */
export function beginDrag(event: PointerEvent | React.PointerEvent, options: DragOptions) {
  if (event.button !== 0) return;
  const startX = event.clientX;
  const startY = event.clientY;
  const pointerId = event.pointerId;
  let ghost: HTMLDivElement | null = null;
  let over: HTMLElement | null = null;
  let edge: DropEdge | null = null;

  const clearOver = () => {
    if (over) delete over.dataset.dropOver;
    over = null;
    edge = null;
  };

  const move = (e: PointerEvent) => {
    if (e.pointerId !== pointerId) return;
    if (!ghost) {
      if (Math.hypot(e.clientX - startX, e.clientY - startY) < THRESHOLD) return;
      ghost = document.createElement('div');
      ghost.className = 'drag-ghost';
      ghost.textContent = options.label;
      document.body.append(ghost);
      document.documentElement.dataset.dragging = 'true';
      options.onStart?.();
    }
    ghost.style.transform = `translate(${e.clientX + 12}px, ${e.clientY + 10}px)`;

    let found: HTMLElement | null = null;
    let foundEdge: DropEdge | null = null;
    for (const element of document.elementsFromPoint(e.clientX, e.clientY)) {
      if (!(element instanceof HTMLElement) || element === ghost) continue;
      const candidate = element.closest<HTMLElement>('[data-drop]');
      if (!candidate) continue;
      const accepted = options.accept(candidate, e.clientY);
      if (accepted) {
        found = candidate;
        foundEdge = accepted;
        break;
      }
    }
    if (found !== over || foundEdge !== edge) {
      clearOver();
      if (found && foundEdge) {
        over = found;
        edge = foundEdge;
        found.dataset.dropOver = foundEdge;
      }
    }
  };

  const finish = (e: PointerEvent, drop: boolean) => {
    if (e.pointerId !== pointerId) return;
    window.removeEventListener('pointermove', move, true);
    window.removeEventListener('pointerup', up, true);
    window.removeEventListener('pointercancel', cancel, true);
    window.removeEventListener('keydown', escape, true);
    if (!ghost) return;
    ghost.remove();
    delete document.documentElement.dataset.dragging;
    // The click that follows a drag must not also open or select something.
    const swallow = (click: MouseEvent) => {
      click.stopPropagation();
      click.preventDefault();
    };
    window.addEventListener('click', swallow, { capture: true, once: true });
    window.setTimeout(() => window.removeEventListener('click', swallow, true), 0);
    const target = over && edge ? { element: over, data: over.dataset, edge } : null;
    clearOver();
    options.onEnd?.();
    if (drop && target) options.onDrop(target);
  };
  const up = (e: PointerEvent) => finish(e, true);
  const cancel = (e: PointerEvent) => finish(e, false);
  const escape = (e: KeyboardEvent) => {
    if (e.key !== 'Escape' || !ghost) return;
    e.preventDefault();
    e.stopPropagation();
    finish(new PointerEvent('pointercancel', { pointerId }), false);
  };

  window.addEventListener('pointermove', move, true);
  window.addEventListener('pointerup', up, true);
  window.addEventListener('pointercancel', cancel, true);
  window.addEventListener('keydown', escape, true);
}

/** Upper half of a row means "before it", lower half "after it". */
export function edgeByHalf(element: HTMLElement, pointerY: number): 'before' | 'after' {
  const rect = element.getBoundingClientRect();
  return pointerY < rect.top + rect.height / 2 ? 'before' : 'after';
}
