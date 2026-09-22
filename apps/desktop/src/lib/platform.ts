/**
 * Which system the app runs on, for the few words and keys that differ.
 *
 * The webview's user agent says it plainly on all three: WebView2 on Windows,
 * WKWebView on macOS, WebKitGTK on Linux.
 */

export type Platform = 'windows' | 'macos' | 'linux';

export function platform(): Platform {
  const agent = `${navigator.userAgent} ${navigator.platform ?? ''}`.toLowerCase();
  if (agent.includes('win')) return 'windows';
  if (agent.includes('mac')) return 'macos';
  return 'linux';
}

/** "Windows", "macOS" or "Linux", as the system calls itself. */
export function systemName(): string {
  switch (platform()) {
    case 'windows':
      return 'Windows';
    case 'macos':
      return 'macOS';
    default:
      return 'Linux';
  }
}
