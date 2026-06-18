#!/usr/bin/env python3
"""Generate theme-aware status-color channel vars + tailwind config.

Dark channels are byte-identical to Tailwind's default palette (zero dark
regression). Light channels use a contrast-biased shade inversion so that
text shades (300-500) become dark and background shades (800-950) become
light — fixing every status usage in light mode with zero markup changes.
Same technique already proven on the `cortex` scale.
"""

# Tailwind v3.4 default palette (hex) for the colors actually used in the app.
PAL = {
    "red":     {50:"fef2f2",100:"fee2e2",200:"fecaca",300:"fca5a5",400:"f87171",500:"ef4444",600:"dc2626",700:"b91c1c",800:"991b1b",900:"7f1d1d",950:"450a0a"},
    "amber":   {50:"fffbeb",100:"fef3c7",200:"fde68a",300:"fcd34d",400:"fbbf24",500:"f59e0b",600:"d97706",700:"b45309",800:"92400e",900:"78350f",950:"451a03"},
    "emerald": {50:"ecfdf5",100:"d1fae5",200:"a7f3d0",300:"6ee7b7",400:"34d399",500:"10b981",600:"059669",700:"047857",800:"065f46",900:"064e3b",950:"022c22"},
    "cyan":    {50:"ecfeff",100:"cffafe",200:"a5f3fc",300:"67e8f9",400:"22d3ee",500:"06b6d4",600:"0891b2",700:"0e7490",800:"155e75",900:"164e63",950:"083344"},
    "indigo":  {50:"eef2ff",100:"e0e7ff",200:"c7d2fe",300:"a5b4fc",400:"818cf8",500:"6366f1",600:"4f46e5",700:"4338ca",800:"3730a3",900:"312e81",950:"1e1b4b"},
    "orange":  {50:"fff7ed",100:"ffedd5",200:"fed7aa",300:"fdba74",400:"fb923c",500:"f97316",600:"ea580c",700:"c2410c",800:"9a3412",900:"7c2d12",950:"431407"},
    "blue":    {50:"eff6ff",100:"dbeafe",200:"bfdbfe",300:"93c5fd",400:"60a5fa",500:"3b82f6",600:"2563eb",700:"1d4ed8",800:"1e40af",900:"1e3a8a",950:"172554"},
}

SHADES = [50,100,200,300,400,500,600,700,800,900,950]

# Light-mode shade remap: text-ish shades (300-500) go dark for contrast on
# light surfaces; bg-ish shades (800-950) go light so opacity tints read with
# dark text. Monotonic so hover/active relationships are preserved.
LIGHT_MAP = {50:950,100:900,200:800,300:700,400:700,500:700,600:300,700:200,800:200,900:100,950:50}

def hex_to_rgb(h):
    return f"{int(h[0:2],16)} {int(h[2:4],16)} {int(h[4:6],16)}"

def channels(light):
    out = []
    for color, pal in PAL.items():
        parts = []
        for s in SHADES:
            src = pal[LIGHT_MAP[s]] if light else pal[s]
            parts.append(f"--{color}-{s}-rgb:{hex_to_rgb(src)};")
        out.append("  " + " ".join(parts))
    return "\n".join(out)

def tw_config():
    lines = []
    for color in PAL:
        entries = ", ".join(f'{s}: "rgb(var(--{color}-{s}-rgb) / <alpha-value>)"' for s in SHADES)
        lines.append(f"        {color}: {{ {entries} }},")
    return "\n".join(lines)

print("=== DARK (:root) ===")
print(channels(False))
print("\n=== LIGHT (.light) ===")
print(channels(True))
print("\n=== TAILWIND CONFIG ===")
print(tw_config())
