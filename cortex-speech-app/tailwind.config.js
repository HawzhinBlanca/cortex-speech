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
          50: "rgb(var(--cortex-50-rgb) / <alpha-value>)",
          100: "rgb(var(--cortex-100-rgb) / <alpha-value>)",
          200: "rgb(var(--cortex-200-rgb) / <alpha-value>)",
          300: "rgb(var(--cortex-300-rgb) / <alpha-value>)",
          400: "rgb(var(--cortex-400-rgb) / <alpha-value>)",
          500: "rgb(var(--cortex-500-rgb) / <alpha-value>)",
          600: "rgb(var(--cortex-600-rgb) / <alpha-value>)",
          700: "rgb(var(--cortex-700-rgb) / <alpha-value>)",
          800: "rgb(var(--cortex-800-rgb) / <alpha-value>)",
          900: "rgb(var(--cortex-900-rgb) / <alpha-value>)",
          950: "rgb(var(--cortex-950-rgb) / <alpha-value>)",
        },
        /* Status palettes — theme-aware (channels defined in app.css :root/.light).
           Overriding the defaults makes every existing red/amber/emerald/cyan/
           indigo/orange/blue utility switch correctly in light mode for free. */
        red: { 50: "rgb(var(--red-50-rgb) / <alpha-value>)", 100: "rgb(var(--red-100-rgb) / <alpha-value>)", 200: "rgb(var(--red-200-rgb) / <alpha-value>)", 300: "rgb(var(--red-300-rgb) / <alpha-value>)", 400: "rgb(var(--red-400-rgb) / <alpha-value>)", 500: "rgb(var(--red-500-rgb) / <alpha-value>)", 600: "rgb(var(--red-600-rgb) / <alpha-value>)", 700: "rgb(var(--red-700-rgb) / <alpha-value>)", 800: "rgb(var(--red-800-rgb) / <alpha-value>)", 900: "rgb(var(--red-900-rgb) / <alpha-value>)", 950: "rgb(var(--red-950-rgb) / <alpha-value>)" },
        amber: { 50: "rgb(var(--amber-50-rgb) / <alpha-value>)", 100: "rgb(var(--amber-100-rgb) / <alpha-value>)", 200: "rgb(var(--amber-200-rgb) / <alpha-value>)", 300: "rgb(var(--amber-300-rgb) / <alpha-value>)", 400: "rgb(var(--amber-400-rgb) / <alpha-value>)", 500: "rgb(var(--amber-500-rgb) / <alpha-value>)", 600: "rgb(var(--amber-600-rgb) / <alpha-value>)", 700: "rgb(var(--amber-700-rgb) / <alpha-value>)", 800: "rgb(var(--amber-800-rgb) / <alpha-value>)", 900: "rgb(var(--amber-900-rgb) / <alpha-value>)", 950: "rgb(var(--amber-950-rgb) / <alpha-value>)" },
        emerald: { 50: "rgb(var(--emerald-50-rgb) / <alpha-value>)", 100: "rgb(var(--emerald-100-rgb) / <alpha-value>)", 200: "rgb(var(--emerald-200-rgb) / <alpha-value>)", 300: "rgb(var(--emerald-300-rgb) / <alpha-value>)", 400: "rgb(var(--emerald-400-rgb) / <alpha-value>)", 500: "rgb(var(--emerald-500-rgb) / <alpha-value>)", 600: "rgb(var(--emerald-600-rgb) / <alpha-value>)", 700: "rgb(var(--emerald-700-rgb) / <alpha-value>)", 800: "rgb(var(--emerald-800-rgb) / <alpha-value>)", 900: "rgb(var(--emerald-900-rgb) / <alpha-value>)", 950: "rgb(var(--emerald-950-rgb) / <alpha-value>)" },
        cyan: { 50: "rgb(var(--cyan-50-rgb) / <alpha-value>)", 100: "rgb(var(--cyan-100-rgb) / <alpha-value>)", 200: "rgb(var(--cyan-200-rgb) / <alpha-value>)", 300: "rgb(var(--cyan-300-rgb) / <alpha-value>)", 400: "rgb(var(--cyan-400-rgb) / <alpha-value>)", 500: "rgb(var(--cyan-500-rgb) / <alpha-value>)", 600: "rgb(var(--cyan-600-rgb) / <alpha-value>)", 700: "rgb(var(--cyan-700-rgb) / <alpha-value>)", 800: "rgb(var(--cyan-800-rgb) / <alpha-value>)", 900: "rgb(var(--cyan-900-rgb) / <alpha-value>)", 950: "rgb(var(--cyan-950-rgb) / <alpha-value>)" },
        indigo: { 50: "rgb(var(--indigo-50-rgb) / <alpha-value>)", 100: "rgb(var(--indigo-100-rgb) / <alpha-value>)", 200: "rgb(var(--indigo-200-rgb) / <alpha-value>)", 300: "rgb(var(--indigo-300-rgb) / <alpha-value>)", 400: "rgb(var(--indigo-400-rgb) / <alpha-value>)", 500: "rgb(var(--indigo-500-rgb) / <alpha-value>)", 600: "rgb(var(--indigo-600-rgb) / <alpha-value>)", 700: "rgb(var(--indigo-700-rgb) / <alpha-value>)", 800: "rgb(var(--indigo-800-rgb) / <alpha-value>)", 900: "rgb(var(--indigo-900-rgb) / <alpha-value>)", 950: "rgb(var(--indigo-950-rgb) / <alpha-value>)" },
        orange: { 50: "rgb(var(--orange-50-rgb) / <alpha-value>)", 100: "rgb(var(--orange-100-rgb) / <alpha-value>)", 200: "rgb(var(--orange-200-rgb) / <alpha-value>)", 300: "rgb(var(--orange-300-rgb) / <alpha-value>)", 400: "rgb(var(--orange-400-rgb) / <alpha-value>)", 500: "rgb(var(--orange-500-rgb) / <alpha-value>)", 600: "rgb(var(--orange-600-rgb) / <alpha-value>)", 700: "rgb(var(--orange-700-rgb) / <alpha-value>)", 800: "rgb(var(--orange-800-rgb) / <alpha-value>)", 900: "rgb(var(--orange-900-rgb) / <alpha-value>)", 950: "rgb(var(--orange-950-rgb) / <alpha-value>)" },
        blue: { 50: "rgb(var(--blue-50-rgb) / <alpha-value>)", 100: "rgb(var(--blue-100-rgb) / <alpha-value>)", 200: "rgb(var(--blue-200-rgb) / <alpha-value>)", 300: "rgb(var(--blue-300-rgb) / <alpha-value>)", 400: "rgb(var(--blue-400-rgb) / <alpha-value>)", 500: "rgb(var(--blue-500-rgb) / <alpha-value>)", 600: "rgb(var(--blue-600-rgb) / <alpha-value>)", 700: "rgb(var(--blue-700-rgb) / <alpha-value>)", 800: "rgb(var(--blue-800-rgb) / <alpha-value>)", 900: "rgb(var(--blue-900-rgb) / <alpha-value>)", 950: "rgb(var(--blue-950-rgb) / <alpha-value>)" },
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
