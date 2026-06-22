/** @type {import('tailwindcss').Config} */
export default {
  content: ["./index.html", "./src/**/*.{ts,tsx}"],
  theme: {
    extend: {
      // Bundled font first so layout/metrics are identical on every OS,
      // regardless of which system fonts are installed (docs/tech-stack.md §5).
      fontFamily: {
        sans: ['"Inter"', "ui-sans-serif", "system-ui", "sans-serif"],
        mono: ['"JetBrains Mono"', "ui-monospace", "monospace"],
      },
      colors: {
        // Custom control palette (WCAG 2.2 AA contrast checked for text/bg).
        // Backed by CSS variables (src/styles/global.css) so the app can switch
        // between the dark (:root) and light (:root.theme-light) palettes by
        // toggling a class on <html>; see src/lib/theme-preference.ts.
        ink: {
          DEFAULT: "var(--ink)",
          soft: "var(--ink-soft)",
          card: "var(--ink-card)",
          line: "var(--ink-line)",
        },
        accent: {
          DEFAULT: "var(--accent)",
          hover: "var(--accent-hover)",
          soft: "var(--accent-soft)",
        },
        muted: "var(--muted)",
        canvas: "var(--canvas)",
        // Semantic foreground tokens — theme-aware text colors that replace the
        // hardcoded near-white classes (text-slate-100/200, text-white).
        fg: {
          DEFAULT: "var(--fg)",
          strong: "var(--fg-strong)",
        },
        // Text on an accent fill; constant white across themes.
        "on-accent": "var(--on-accent)",
      },
      boxShadow: {
        glow: "0 0 0 2px var(--accent), 0 0 18px 2px rgba(79,140,255,0.45)",
      },
    },
  },
  plugins: [],
};
