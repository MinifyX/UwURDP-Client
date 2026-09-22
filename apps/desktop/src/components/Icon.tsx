/**
 * Small line icons for buttons and menus, drawn on a 24 × 24 grid with round
 * caps like the title bar's gear. Inline SVG: the app loads nothing from
 * outside.
 */

const PATHS = {
  house: 'M4 11.5 12 5l8 6.5 M6.5 9.5V19h11V9.5 M10 19v-5h4v5',
  briefcase: 'M4 8h16v11H4z M9 8V6a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2 M4 13h16',
  folder: 'M3.5 6.5a1 1 0 0 1 1-1h4.2l2 2h8.8a1 1 0 0 1 1 1V18a1 1 0 0 1-1 1h-15a1 1 0 0 1-1-1Z',
  folderPlus:
    'M3.5 6.5a1 1 0 0 1 1-1h4.2l2 2h8.8a1 1 0 0 1 1 1V18a1 1 0 0 1-1 1h-15a1 1 0 0 1-1-1Z M12 10.5v5 M9.5 13h5',
  file: 'M6.5 3.5h7l4 4V20a.5.5 0 0 1-.5.5H6.5a.5.5 0 0 1-.5-.5V4a.5.5 0 0 1 .5-.5Z M13.5 3.5v4h4',
  files:
    'M8 3.5h6.5l3.5 3.5V17a.5.5 0 0 1-.5.5H8a.5.5 0 0 1-.5-.5V4a.5.5 0 0 1 .5-.5Z M14.5 3.5V7H18 M5 7v12.5a1 1 0 0 0 1 1h9',
  plus: 'M12 5v14 M5 12h14',
  chevron: 'M9 6l6 6-6 6',
  up: 'M12 19V6 M6 12l6-6 6 6',
  refresh: 'M19.5 12a7.5 7.5 0 1 1-2.2-5.3 M19.5 4.5v4h-4',
  trash: 'M4.5 7h15 M9.5 7V5h5v2 M6.5 7l1 12.5h9l1-12.5 M10 11v5 M14 11v5',
  pencil: 'M15.5 4.5l4 4L9 19H5v-4Z M13.5 6.5l4 4',
  download: 'M12 4v11 M7 10.5l5 5 5-5 M5 19.5h14',
  upload: 'M12 16V5 M7 9.5l5-5 5 5 M5 19.5h14',
  key: 'M14.5 9.5a4 4 0 1 1-2.8-3.8 M11.7 5.7 20 14v3h-3v-2h-2v-2l-1.8-1.8 M8.5 11a1 1 0 1 0 0-.1',
  lock: 'M6 11h12v9H6z M8.5 11V8a3.5 3.5 0 0 1 7 0v3 M12 14.5v2',
  unlock: 'M6 11h12v9H6z M8.5 11V8a3.5 3.5 0 0 1 6.8-1.2 M12 14.5v2',
  shield: 'M12 3.5 19 6v5.5c0 4.3-3 7.6-7 9-4-1.4-7-4.7-7-9V6Z M9 12l2 2 4-4',
  crown: 'M4 17.5h16 M4.5 8l4 4L12 6l3.5 6 4-4-1.5 9.5h-12Z',
  terminal: 'M4 5h16v14H4z M7.5 9.5l3 2.5-3 2.5 M12.5 15h4',
  copy: 'M9 9h10.5v10.5H9z M15 9V4.5H4.5V15H9',
  check: 'M5 12.5l4.5 4.5L19 7.5',
  close: 'M6 6l12 12 M18 6 6 18',
  more: 'M6 12h.01 M12 12h.01 M18 12h.01',
  export: 'M12 3.5v11 M7.5 8l4.5-4.5L16.5 8 M5 13v6.5h14V13',
  import: 'M12 14.5v-11 M7.5 10l4.5 4.5 4.5-4.5 M5 13v6.5h14V13',
  sparkles:
    'M11 4.5c.6 3.4 2.1 4.9 5.5 5.5-3.4.6-4.9 2.1-5.5 5.5-.6-3.4-2.1-4.9-5.5-5.5 3.4-.6 4.9-2.1 5.5-5.5Z M18 14.5c.3 1.6 1 2.3 2.5 2.5-1.6.3-2.3 1-2.5 2.5-.3-1.5-1-2.2-2.5-2.5 1.5-.2 2.2-.9 2.5-2.5Z',
  network: 'M4 6h16v5H4z M4 13h16v5H4z M7.5 8.5h.01 M7.5 15.5h.01',
  eye: 'M2.5 12s3.5-6.5 9.5-6.5 9.5 6.5 9.5 6.5-3.5 6.5-9.5 6.5S2.5 12 2.5 12Z M12 14.5a2.5 2.5 0 1 0 0-5 2.5 2.5 0 0 0 0 5Z',
  home: 'M4 11.5 12 5l8 6.5 M6.5 9.5V19h11V9.5',
  drive: 'M3.5 13.5 6 6h12l2.5 7.5 M3.5 13.5h17v5h-17z M16.5 16h.01',
  stop: 'M7 7h10v10H7z',
  monitor: 'M3.5 4.5h17v11.5h-17z M9 20h6 M12 16v4',
  grid: 'M4 4h7v7H4z M13 4h7v7h-7z M4 13h7v7H4z M13 13h7v7h-7z',
  power: 'M12 4v8 M7.5 7.2a7 7 0 1 0 9 0',
  fullscreen: 'M4 9V4h5 M15 4h5v5 M20 15v5h-5 M9 20H4v-5',
  user: 'M12 12a4 4 0 1 0 0-8 4 4 0 0 0 0 8Z M4.5 20a7.5 7.5 0 0 1 15 0',
  scale: 'M14 4h6v6 M10 20H4v-6 M20 4l-6.5 6.5 M4 20l6.5-6.5',
  keyboard: 'M3.5 7h17v10h-17z M7 10.5h.01 M10.5 10.5h.01 M14 10.5h.01 M17 10.5h.01 M8 14h8',
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({
  name,
  size = 16,
  title,
  className,
}: {
  name: IconName;
  size?: number;
  title?: string;
  className?: string;
}) {
  return (
    <svg
      viewBox="0 0 24 24"
      width={size}
      height={size}
      className={className ? `icon ${className}` : 'icon'}
      role={title ? 'img' : undefined}
      aria-label={title}
      aria-hidden={title ? undefined : true}
      focusable="false"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}
