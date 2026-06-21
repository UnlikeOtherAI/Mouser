interface SectionCardProps {
  title: string;
  description?: string;
  children: React.ReactNode;
}

/** A titled card used to group related settings within a section. */
export function SectionCard({
  title,
  description,
  children,
}: SectionCardProps): React.JSX.Element {
  return (
    <section className="rounded-xl border border-ink-line bg-ink-card px-5 py-1">
      <header className="border-b border-ink-line py-4">
        <h2 className="text-sm font-semibold uppercase tracking-wide text-muted">
          {title}
        </h2>
        {description ? (
          <p className="mt-1 text-sm text-muted">{description}</p>
        ) : null}
      </header>
      <div className="divide-y divide-ink-line">{children}</div>
    </section>
  );
}
