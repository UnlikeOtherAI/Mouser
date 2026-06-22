import { cx } from "../lib/cx";

interface ToggleProps {
  checked: boolean;
  onChange: (next: boolean) => void;
  label: string;
  /** When true the visible label is rendered elsewhere; this stays for a11y. */
  labelHidden?: boolean;
  disabled?: boolean;
}

/**
 * Custom-styled switch (not a native checkbox) so it looks identical on every
 * platform. Implemented as an ARIA `switch` button with full keyboard support.
 */
export function Toggle({
  checked,
  onChange,
  label,
  labelHidden = false,
  disabled = false,
}: ToggleProps): React.JSX.Element {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={labelHidden ? label : undefined}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cx(
        "relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition-colors",
        "focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent",
        disabled && "cursor-not-allowed opacity-50",
        checked ? "bg-accent" : "bg-ink-line",
      )}
    >
      <span
        aria-hidden="true"
        className={cx(
          "inline-block h-5 w-5 transform rounded-full bg-fg-strong shadow transition-transform",
          checked ? "translate-x-5" : "translate-x-0.5",
        )}
      />
    </button>
  );
}
