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
        ink: {
          DEFAULT: "#0f121c",
          soft: "#161a26",
          card: "#1c2130",
          line: "#2a3142",
        },
        accent: {
          DEFAULT: "#4f8cff",
          hover: "#6aa0ff",
          soft: "#23314f",
        },
        muted: "#8a93a6",
        canvas: "#3a3f4b",
      },
      boxShadow: {
        glow: "0 0 0 2px #4f8cff, 0 0 18px 2px rgba(79,140,255,0.45)",
      },
    },
  },
  plugins: [],
};
