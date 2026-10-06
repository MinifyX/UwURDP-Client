/**
 * ⌘ inside a remote desktop on a Mac.
 *
 * Like Microsoft's Windows App, the desktop gets the keyboard: ⌘ is the
 * Windows key there, and ⌘ with a key reaches the server as Windows+key, as
 * it always did. Only the app's own commands stay with the Mac, so the menu
 * bar (App.tsx) or the app's key handler can take them:
 *
 * - ⌘Q quit, ⌘W close the tab, ⌘, settings
 * - ⇧⌘1 … 9 switch tabs
 * - ⇧⌘↩ the desktop's full screen, ⌃⌘F the window's
 *
 * Plain data, no DOM: test/mac-keys.test.mjs runs it in Node.
 */

type KeyLike = Pick<KeyboardEvent, 'code' | 'metaKey' | 'ctrlKey' | 'altKey' | 'shiftKey'>;

/** Whether a key pressed in the desktop belongs to the app rather than the server. */
export function macAppShortcut(event: KeyLike): boolean {
  if (!event.metaKey || event.altKey) return false;
  const { code, ctrlKey, shiftKey } = event;
  if (ctrlKey) return !shiftKey && code === 'KeyF';
  if (!shiftKey) return code === 'KeyQ' || code === 'KeyW' || code === 'Comma';
  return code === 'Enter' || /^Digit[1-9]$/.test(code);
}
