/** @type {import('tailwindcss').Config} */
export default {
  darkMode: "class",
  content: ["./src/**/*.{html,js,svelte,ts,css}", "./index.html"],
  theme: {
    extend: {
      fontFamily: {
        sans: ["Inter", "Vazirmatn", "system-ui", "-apple-system", "Segoe UI", "Roboto", "sans-serif"],
        mono: ["JetBrains Mono", "ui-monospace", "SFMono-Regular", "Menlo", "monospace"],
        kurdish: ["Vazirmatn", "Inter", "system-ui", "sans-serif"],
      },
      colors: {
        /* Refined brand scale — surfaces (800–950) align to the CSS-var surfaces,
           so existing bg-cortex-* / border-cortex-* classes upgrade for free. */
        cortex: {
          50: "#eef5fb",
          100: "#dde9f3",
          200: "#bdd4e7",
          300: "#90b6d3",
          400: "#5d94bc",
          500: "#3e79a3",
          600: "#2f5d83",
          700: "#22384e",
          800: "#19212f",
          900: "#0d121b",
          950: "#090d14",
        },
        /* Semantic tokens (drive the design system) */
        app: "var(--app-bg)",
        surface: {
          DEFAULT: "var(--surface-1)",
          1: "var(--surface-1)",
          2: "var(--surface-2)",
          3: "var(--surface-3)",
          inset: "var(--surface-inset)",
        },
        accent: {
          DEFAULT: "var(--accent)",
          strong: "var(--accent-strong)",
          soft: "var(--accent-soft)",
        },
        line: "var(--border)",
        "line-strong": "var(--border-strong)",
        default: "var(--text)",
        muted: "var(--text-muted)",
        subtle: "var(--text-subtle)",
        success: "var(--success)",
        warning: "var(--warning)",
        danger: "var(--danger)",
        info: "var(--info)",
      },
      boxShadow: {
        soft: "var(--shadow-md)",
        lift: "var(--shadow-lg)",
        glow: "0 0 0 1px var(--accent-soft), 0 10px 40px -10px rgba(14,165,233,0.4)",
      },
      borderRadius: {
        token: "var(--r-md)",
        "token-lg": "var(--r-lg)",
        "token-xl": "var(--r-xl)",
      },
      transitionTimingFunction: {
        smooth: "cubic-bezier(0.32, 0.72, 0, 1)",
        "out-quint": "cubic-bezier(0.16, 1, 0.3, 1)",
      },
      keyframes: {
        "fade-in": { from: { opacity: "0" }, to: { opacity: "1" } },
        "slide-up": { from: { opacity: "0", transform: "translateY(8px)" }, to: { opacity: "1", transform: "translateY(0)" } },
        "scale-in": { from: { opacity: "0", transform: "scale(0.97)" }, to: { opacity: "1", transform: "scale(1)" } },
      },
      animation: {
        "fade-in": "fade-in var(--dur-2) var(--ease-out) both",
        "slide-up": "slide-up var(--dur-3) var(--ease-out) both",
        "scale-in": "scale-in var(--dur-2) var(--ease-out) both",
      },
    },
  },
  plugins: [],
};
