interface SettingRowProps {
  title: string;
  description?: string;
  /** The control (toggle, segmented, etc.) rendered on the right. */
  control: React.ReactNode;
  /** Associates the row label with its control for assistive tech. */
  htmlFor?: string;
}

/** Consistent label + description + control row used across settings sections. */
export function SettingRow({
  title,
  description,
  control,
  htmlFor,
}: SettingRowProps): React.JSX.Element {
  return (
    <div className="flex items-start justify-between gap-6 py-4">
      <div className="min-w-0">
        <label
          htmlFor={htmlFor}
          className="block text-sm font-medium text-slate-100"
        >
          {title}
        </label>
        {description ? (
          <p className="mt-1 text-sm text-muted">{description}</p>
        ) : null}
      </div>
      <div className="shrink-0 pt-0.5">{control}</div>
    </div>
  );
}
