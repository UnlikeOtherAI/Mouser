// Small formatting helpers for byte sizes shown in the clipboard UI (§7.7).
// Binary units (KiB/MiB/GiB) to match the spec's chunk sizing language.

const UNITS = ["B", "KiB", "MiB", "GiB", "TiB"] as const;

/** Human-readable byte size, e.g. `0` → "0 B", `1572864` → "1.5 MiB". */
export function formatBytes(bytes: number): string {
  if (bytes <= 0) return "0 B";
  const exponent = Math.min(
    Math.floor(Math.log2(bytes) / 10),
    UNITS.length - 1,
  );
  const value = bytes / 1024 ** exponent;
  const unit = UNITS[exponent] ?? "B";
  // Whole numbers and the base unit stay integer; otherwise one decimal.
  const rounded =
    exponent === 0 || Number.isInteger(value) ? value : value.toFixed(1);
  return `${rounded} ${unit}`;
}
