import { cx } from "../lib/cx";
import { formatBytes } from "../lib/format";
import type { ClipboardTransfer, ClipFormat } from "../lib/types";

const FORMAT_GLYPH: Record<ClipFormat, string> = {
  text: "📄",
  image: "🖼️",
  files: "📎",
};

const FORMAT_NOUN: Record<ClipFormat, string> = {
  text: "text",
  image: "image",
  files: "files",
};

/** Verb + peer line, e.g. "Pasting from Game Rig…" / "Sending to Studio Mac…". */
function transferLabel(transfer: ClipboardTransfer): string {
  const verb = transfer.direction === "incoming" ? "Pasting from" : "Sending to";
  if (transfer.state === "done") {
    return transfer.direction === "incoming"
      ? `Pasted from ${transfer.peer}`
      : `Sent to ${transfer.peer}`;
  }
  if (transfer.state === "failed") {
    return transfer.direction === "incoming"
      ? `Failed to paste from ${transfer.peer}`
      : `Failed to send to ${transfer.peer}`;
  }
  return `${verb} ${transfer.peer}…`;
}

function percentFor(transfer: ClipboardTransfer): number {
  if (transfer.state === "done") return 100;
  if (transfer.total <= 0) return 0;
  return Math.min(100, Math.round((transfer.received / transfer.total) * 100));
}

/** A single transfer row: glyph, label, determinate bar, and byte/percent meta. */
function TransferToast({
  transfer,
}: {
  transfer: ClipboardTransfer;
}): React.JSX.Element {
  const percent = percentFor(transfer);
  const failed = transfer.state === "failed";
  const done = transfer.state === "done";

  return (
    <div className="w-72 rounded-xl border border-ink-line bg-ink-card px-4 py-3 shadow-lg">
      <div className="flex items-center gap-2">
        <span aria-hidden="true" className="text-base">
          {FORMAT_GLYPH[transfer.format]}
        </span>
        <p className="min-w-0 flex-1 truncate text-sm font-medium text-slate-100">
          {transferLabel(transfer)}
        </p>
        {!done && !failed ? (
          <span className="shrink-0 text-xs font-medium tabular-nums text-muted">
            {percent}%
          </span>
        ) : null}
      </div>

      <div
        className="mt-2 h-1.5 w-full overflow-hidden rounded-full bg-ink-soft"
        role="progressbar"
        aria-label={transferLabel(transfer)}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={percent}
      >
        <div
          className={cx(
            "h-full rounded-full transition-[width] duration-300",
            failed ? "bg-rose-500" : done ? "bg-emerald-400" : "bg-accent",
          )}
          style={{ width: `${failed ? 100 : percent}%` }}
        />
      </div>

      <p className="mt-1.5 text-xs text-muted">
        {failed
          ? `Transfer failed · ${FORMAT_NOUN[transfer.format]}`
          : done
            ? `${formatBytes(transfer.total)} · done`
            : `${formatBytes(transfer.received)} of ${formatBytes(transfer.total)}`}
      </p>
    </div>
  );
}

/**
 * Mac-style Universal Clipboard "wait" indicator (§7.7). Renders in-flight
 * clipboard transfers as a stacked toast overlay. Driven by UI-local
 * `ClipboardTransfer` data now; wired to the engine's progress events
 * (`reassembly::Progress`) once IPC lands.
 */
export function ClipboardProgress({
  transfers,
}: {
  transfers: ClipboardTransfer[];
}): React.JSX.Element | null {
  if (transfers.length === 0) return null;

  return (
    <div
      aria-live="polite"
      aria-label="Clipboard transfers"
      className="pointer-events-none fixed bottom-4 right-4 z-50 flex flex-col gap-2"
    >
      {transfers.map((transfer) => (
        <TransferToast key={transfer.id} transfer={transfer} />
      ))}
    </div>
  );
}
