import { cx } from "../lib/cx";
import type { NavItem, SectionId } from "../lib/types";

interface SideNavProps {
  items: NavItem[];
  active: SectionId;
  onSelect: (id: SectionId) => void;
}

/**
 * Left navigation rail. Rendered as a single-select `tablist` so the main
 * content can be the corresponding `tabpanel`; arrow keys move between tabs.
 */
export function SideNav({
  items,
  active,
  onSelect,
}: SideNavProps): React.JSX.Element {
  function onTabKeyDown(event: React.KeyboardEvent<HTMLButtonElement>): void {
    const index = items.findIndex((i) => i.id === active);
    if (index < 0) return;
    let next = index;
    if (event.key === "ArrowDown") next = (index + 1) % items.length;
    else if (event.key === "ArrowUp")
      next = (index - 1 + items.length) % items.length;
    else return;
    event.preventDefault();
    const target = items[next];
    if (!target) return;
    onSelect(target.id);
    // Move focus to the newly selected tab (WAI-ARIA tabs pattern).
    event.currentTarget.parentElement
      ?.querySelector<HTMLButtonElement>(`#tab-${target.id}`)
      ?.focus();
  }

  return (
    <nav
      aria-label="Settings sections"
      className="flex h-full w-56 shrink-0 flex-col border-r border-ink-line bg-ink-soft"
    >
      <div className="flex items-center gap-2 px-4 py-4">
        <span aria-hidden="true" className="text-lg">
          🖱️
        </span>
        <span className="text-base font-semibold tracking-tight">Mouser</span>
      </div>
      <div role="tablist" aria-orientation="vertical" className="px-2">
        {items.map((item) => {
          const selected = item.id === active;
          return (
            <button
              key={item.id}
              type="button"
              role="tab"
              id={`tab-${item.id}`}
              aria-selected={selected}
              aria-controls={`panel-${item.id}`}
              tabIndex={selected ? 0 : -1}
              onClick={() => onSelect(item.id)}
              onKeyDown={onTabKeyDown}
              className={cx(
                "mb-0.5 flex w-full items-center rounded-lg px-3 py-2 text-left text-sm font-medium transition-colors",
                "focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent",
                selected
                  ? "bg-accent-soft text-slate-100"
                  : "text-muted hover:bg-ink-line/60 hover:text-slate-200",
              )}
            >
              {item.label}
            </button>
          );
        })}
      </div>
    </nav>
  );
}
