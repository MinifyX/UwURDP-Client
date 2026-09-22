/**
 * Nyu, the remote-desktop cat.
 *
 * Same cat as in UwUMail and UwUSSH — the envelope is a monitor here, on a
 * little stand, with the screen as the face and a power light where UwUSSH's
 * terminal had its window dots. The screen, eyes, mouth and blush sit exactly
 * where they did, so every mood and scene fits; the stand hangs below the
 * body, inside the sticker, so it gets the white edge too.
 *
 * The colours are fixed artwork, not theme tokens: a sticker looks like itself
 * in dark mode too, and the white die-cut edge is what keeps the outlines
 * readable on a dark ground.
 *
 * Shared with the installer (apps/setup), so nothing in here may depend on the
 * app's stylesheet beyond `nyu.css`.
 */

import type { ReactNode } from 'react';

export const NYU = {
  outline: '#4B1D3F',
  body: '#FF6FA6',
  screen: '#FFB8D3',
  blush: '#FF4D8D',
  edge: '#FFFFFF',
  paper: '#FFFFFF',
  star: '#FFD66E',
  tear: '#9ED8FF',
  lilac: '#C9B6F0',
  violet: '#A78BFA',
  mint: '#B9F0D0',
  sky: '#BDE6FF',
  kraft: '#F2C58F',
  kraftLight: '#F8DDB8',
  tile: '#FFE4EF',
} as const;

export type NyuMood = 'uwu' | 'happy' | 'cheer' | 'sparkle' | 'sad' | 'puzzled' | 'sleepy';

/** Draws its children twice: first as a white die-cut edge, then as they are. */
export function Sticker({ edge, children }: { edge: number; children: ReactNode }) {
  return (
    <>
      <g className="nyu-edge" strokeWidth={edge}>
        {children}
      </g>
      {children}
    </>
  );
}

const line = { fill: 'none', stroke: NYU.outline, strokeWidth: 8 } as const;

const EYES: Record<NyuMood, ReactNode> = {
  uwu: (
    <g {...line}>
      <path d="M88 142 Q102 160 116 142" />
      <path d="M140 142 Q154 160 168 142" />
    </g>
  ),
  happy: (
    <g>
      <g fill={NYU.outline}>
        <ellipse cx="102" cy="148" rx="8" ry="10" />
        <ellipse cx="154" cy="148" rx="8" ry="10" />
      </g>
      <g fill={NYU.paper}>
        <circle cx="105" cy="144" r="3" />
        <circle cx="157" cy="144" r="3" />
      </g>
    </g>
  ),
  cheer: (
    <g {...line}>
      <path d="M92 138 L110 148 L92 158" />
      <path d="M164 138 L146 148 L164 158" />
    </g>
  ),
  sparkle: (
    <g fill={NYU.star} stroke={NYU.outline} strokeWidth={4}>
      <path d="M102 134 Q104 146 116 148 Q104 150 102 162 Q100 150 88 148 Q100 146 102 134Z" />
      <path d="M154 134 Q156 146 168 148 Q156 150 154 162 Q152 150 140 148 Q152 146 154 134Z" />
    </g>
  ),
  sad: (
    <g>
      <g {...line}>
        <path d="M90 152 Q102 142 114 152" />
        <path d="M142 152 Q154 142 166 152" />
      </g>
      <g fill={NYU.tear} stroke={NYU.outline} strokeWidth={4}>
        <path d="M94 158 q-6 9 0 13 q6 -4 0 -13Z" />
        <path d="M162 158 q-6 9 0 13 q6 -4 0 -13Z" />
      </g>
    </g>
  ),
  puzzled: (
    <g fill={NYU.outline}>
      <circle cx="102" cy="148" r="7" />
      <circle cx="154" cy="148" r="7" />
    </g>
  ),
  sleepy: (
    <g {...line}>
      <path d="M90 150 q12 8 24 0" />
      <path d="M142 150 q12 8 24 0" />
    </g>
  ),
};

const W_MOUTH = <path d="M112 170 L120 182 L128 170 L136 182 L144 170" {...line} />;

const MOUTHS: Record<NyuMood, ReactNode> = {
  uwu: W_MOUTH,
  happy: W_MOUTH,
  sparkle: W_MOUTH,
  cheer: (
    <path d="M114 170 Q128 194 142 170 Z" fill={NYU.outline} stroke={NYU.outline} strokeWidth={6} />
  ),
  sad: <path d="M114 184 Q128 172 142 184" {...line} />,
  puzzled: <path d="M114 180 q7 -6 14 0 q7 6 14 0" {...line} strokeWidth={7} />,
  sleepy: <path d="M120 180 q4 5 8 0 q4 5 8 0" {...line} strokeWidth={6} />,
};

/**
 * A paw in Nyu's own coordinates (the body spans 28–228 × 74–220; the stand
 * hangs below it, down to 246).
 */
export function Paw({ x, y, className }: { x: number; y: number; className?: string }) {
  return (
    <g className={className}>
      <ellipse cx={x} cy={y} rx="19" ry="16" fill={NYU.body} stroke={NYU.outline} strokeWidth={9} />
      <path
        d={`M${x - 5} ${y + 3} v6 M${x + 5} ${y + 3} v6`}
        fill="none"
        stroke={NYU.outline}
        strokeWidth={5}
      />
    </g>
  );
}

type FigureProps = {
  mood?: NyuMood;
  /** Centre of the body in the parent's coordinates. */
  x?: number;
  y?: number;
  /** 1 is the size of the app symbol: the body is 200 wide. */
  scale?: number;
  tilt?: number;
  /** Extra parts in Nyu's own coordinates. */
  behind?: ReactNode;
  front?: ReactNode;
  /** Replaces the mood's eyes, e.g. pupils that follow something. The mouth stays the mood's. */
  eyes?: ReactNode;
  /** The white die-cut edge, in Nyu's own coordinates. */
  edge?: number;
};

/** Nyu as a group, for scenes: placed, scaled and tilted in the parent's coordinates. */
export function NyuFigure({
  mood = 'uwu',
  x = 128,
  y = 147,
  scale = 1,
  tilt = 0,
  behind,
  front,
  eyes,
  edge = 20,
}: FigureProps) {
  return (
    <g
      transform={`translate(${x} ${y}) rotate(${tilt}) scale(${scale}) translate(-128 -147)`}
      strokeLinecap="round"
      strokeLinejoin="round"
    >
      <Sticker edge={edge}>
        {behind}
        <g className="nyu-ear nyu-ear-l">
          <path d="M62 88 L80 36 L114 88 Z" fill={NYU.body} stroke={NYU.outline} strokeWidth={9} />
          <path d="M76 82 L84 52 L100 82 Z" fill={NYU.screen} />
        </g>
        <g className="nyu-ear nyu-ear-r">
          <path
            d="M142 88 L176 36 L194 88 Z"
            fill={NYU.body}
            stroke={NYU.outline}
            strokeWidth={9}
          />
          <path d="M156 82 L172 52 L180 82 Z" fill={NYU.screen} />
        </g>
        <g className="nyu-stand">
          <rect
            x={112}
            y={210}
            width={32}
            height={28}
            fill={NYU.body}
            stroke={NYU.outline}
            strokeWidth={9}
          />
          <path
            d="M82 246 Q84 234 100 234 H156 Q172 234 174 246 Z"
            fill={NYU.body}
            stroke={NYU.outline}
            strokeWidth={9}
          />
        </g>
        <rect
          x={28}
          y={74}
          width={200}
          height={146}
          rx={24}
          fill={NYU.body}
          stroke={NYU.outline}
          strokeWidth={9}
        />
        <circle
          className="no-edge nyu-led"
          cx={204}
          cy={96}
          r={5.5}
          fill={NYU.mint}
          stroke={NYU.outline}
          strokeWidth={3}
        />
        <rect
          x={44}
          y={116}
          width={168}
          height={88}
          rx={16}
          fill={NYU.screen}
          stroke={NYU.outline}
          strokeWidth={7}
        />
        {mood !== 'puzzled' && (
          <g className="no-edge" fill={NYU.blush} opacity={0.5}>
            <ellipse cx={74} cy={168} rx={12} ry={7.5} />
            <ellipse cx={182} cy={168} rx={12} ry={7.5} />
          </g>
        )}
        <g className={mood === 'sleepy' ? undefined : 'nyu-eyes'}>{eyes ?? EYES[mood]}</g>
        {MOUTHS[mood]}
        {front}
      </Sticker>
    </g>
  );
}

type NyuProps = {
  size?: number;
  mood?: NyuMood;
  /** Blinking is on by default and stops on its own when motion is reduced. */
  blink?: boolean;
  title?: string;
};

/** The symbol on its own: title bar, empty states, the update hint. */
export function Nyu({ size = 96, mood = 'uwu', blink = true, title = 'Nyu' }: NyuProps) {
  return (
    <svg
      viewBox="0 0 256 256"
      width={size}
      height={size}
      role="img"
      aria-label={title}
      focusable="false"
      className={blink ? 'nyu-host nyu-blink' : 'nyu-host'}
      style={{ overflow: 'visible' }}
    >
      <NyuFigure mood={mood} />
    </svg>
  );
}
