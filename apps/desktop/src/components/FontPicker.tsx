import { FONT_CHOICES, FONT_NAMES, FONT_STACKS, type FontChoice } from '@uwusuite/design';
import type { KeyboardEvent } from 'react';

/**
 * Settings → Appearance → Font: @uwusuite/design's choices (UwU Sans,
 * Manrope, Rubik, DM Sans, the system's), every one shown in itself, as in
 * UwUMail. The arrow keys move the choice, like the package's Segmented.
 */
export function FontPicker({
  value,
  onChange,
  label,
  systemName,
  sample,
}: {
  value: FontChoice;
  onChange: (font: FontChoice) => void;
  /** The group's name for screen readers. */
  label: string;
  /** What the system's font is called ("System"). */
  systemName: string;
  /** A line in each font. */
  sample: string;
}) {
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
    if (!step) return;
    event.preventDefault();
    const index = FONT_CHOICES.indexOf(value);
    const next = (index + step + FONT_CHOICES.length) % FONT_CHOICES.length;
    onChange(FONT_CHOICES[next]!);
    event.currentTarget.querySelectorAll<HTMLButtonElement>('[role=radio]')[next]?.focus();
  };
  return (
    <div className="font-picker" role="radiogroup" aria-label={label} onKeyDown={onKeyDown}>
      {FONT_CHOICES.map((choice) => (
        <button
          key={choice}
          type="button"
          role="radio"
          aria-checked={value === choice}
          tabIndex={value === choice ? 0 : -1}
          onClick={() => onChange(choice)}
          style={{ fontFamily: FONT_STACKS[choice] }}
        >
          <span className="font-picker-name">
            {choice === 'system' ? systemName : FONT_NAMES[choice]}
          </span>
          <span className="font-picker-sample">{sample}</span>
        </button>
      ))}
    </div>
  );
}
