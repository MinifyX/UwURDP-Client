/**
 * Nyu, the remote-desktop cat, from @uwusuite/design: the suite's palette,
 * the sticker edge, the ears and the face are the package's; the monitor
 * shell is UwURDP's. The envelope is a monitor here, on a little stand, with
 * the screen as the face and a power light; the stand hangs below the body,
 * inside the sticker, so it gets the white edge too.
 *
 * `Nyu` on its own (title bar, empty states, the update hint) is the
 * package's `<Nyu shell="monitor">`. `NyuFigure` is the same cat as a group
 * for the scenes, with extra parts behind and in front, eyes that look
 * somewhere, and a place, size and tilt of her own.
 *
 * The colours are fixed artwork, not theme tokens: a sticker looks like itself
 * in dark mode too, and the white die-cut edge is what keeps the outlines
 * readable on a dark ground.
 *
 * Shared with the installer (apps/setup), so nothing in here may depend on the
 * app's stylesheet beyond the package's `nyu.css`.
 */

import { NYU, Nyu as SuiteNyu, NyuEars, NyuFace, Sticker, type NyuMood } from '@uwusuite/design';
import type { ReactNode } from 'react';

export { NYU, Sticker, type NyuMood };

/**
 * A paw in Nyu's own coordinates (the body spans 28–228 × 74–220; the stand
 * hangs below it, down to 246). A little bigger than the package's, to match
 * the monitor's 9 px outline.
 */
export function Paw({ x, y, className }: { x: number; y: number; className?: string }) {
  return (
    <g className={className}>
      <ellipse cx={x} cy={y} rx="19" ry="16" fill={NYU.body} stroke={NYU.ink} strokeWidth={9} />
      <path
        d={`M${x - 5} ${y + 3} v6 M${x + 5} ${y + 3} v6`}
        fill="none"
        stroke={NYU.ink}
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
        <NyuEars />
        <g className="nyu-stand">
          <rect
            x={112}
            y={210}
            width={32}
            height={28}
            fill={NYU.body}
            stroke={NYU.ink}
            strokeWidth={9}
          />
          <path
            d="M82 246 Q84 234 100 234 H156 Q172 234 174 246 Z"
            fill={NYU.body}
            stroke={NYU.ink}
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
          stroke={NYU.ink}
          strokeWidth={9}
        />
        <circle
          className="no-edge nyu-led"
          cx={204}
          cy={96}
          r={5.5}
          fill={NYU.mint}
          stroke={NYU.ink}
          strokeWidth={3}
        />
        <rect
          x={44}
          y={116}
          width={168}
          height={88}
          rx={16}
          fill={NYU.flap}
          stroke={NYU.ink}
          strokeWidth={7}
        />
        <NyuFace mood={mood} eyes={eyes} />
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
  return <SuiteNyu shell="monitor" mood={mood} size={size} blink={blink} title={title} />;
}
