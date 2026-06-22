import { cx } from "../lib/cx";

export interface SegmentedOption<T extends string> {
  value: T;
  label: string;
}

interface SegmentedProps<T extends string> {
  options: ReadonlyArray<SegmentedOption<T>>;
  value: T;
  onChange: (next: T) => void;
  label: string;
  /** When true the whole group is greyed out and non-interactive. */
  disabled?: boolean;
}

/**
 * Custom segmented control (radio group). Native radios are replaced by styled
 * buttons; arrow-key roving is handled by the browser's `radiogroup` semantics
 * via tab + click, with explicit `aria-checked` for assistive tech.
 */
export function Segmented<T extends string>({
  options,
  value,
  onChange,
  label,
  disabled = false,
}: SegmentedProps<T>): React.JSX.Element {
  return (
    <div
      role="radiogroup"
      aria-label={label}
      aria-disabled={disabled || undefined}
      className={cx(
        "inline-flex rounded-lg border border-ink-line bg-ink-soft p-0.5",
        disabled && "cursor-not-allowed opacity-50",
      )}
    >
      {options.map((opt) => {
        const active = opt.value === value;
        return (
          <button
            key={opt.value}
            type="button"
            role="radio"
            aria-checked={active}
            disabled={disabled}
            onClick={() => onChange(opt.value)}
            className={cx(
              "rounded-md px-3 py-1.5 text-sm font-medium transition-colors",
              "focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent",
              disabled && "cursor-not-allowed",
              active
                ? "bg-accent text-on-accent"
                : "text-muted hover:text-fg-strong",
            )}
          >
            {opt.label}
          </button>
        );
      })}
    </div>
  );
}
