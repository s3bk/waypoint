// The page chrome (margin, panels, hint, title) follows the canvas colour:
// a dark canvas gets the dark theme and a light one the light theme, so
// text stays readable against whatever the owner chose. Set as CSS variables.

import { contrast, readableOn } from "./style";

type Rgb = [number, number, number];

const rgb = (c: string): Rgb => [1, 3, 5].map((i) => parseInt(c.slice(i, i + 2), 16)) as Rgb;
const hex = (c: Rgb) => "#" + c.map((v) => Math.round(v).toString(16).padStart(2, "0")).join("");

/** `a` moved `t` (0-1) of the way to `b`. */
function mix(a: string, b: string, t: number): string {
  const [x, y] = [rgb(a), rgb(b)];
  return hex(x.map((v, i) => v + (y[i]! - v) * t) as Rgb);
}

interface MarginPalette {
  bg: string;
  fg: string;
  soft: string;
  muted: string;
  accent: string;
}

/** The margin's four surfaces: two dark, two light, each with text colours that read on it. */
const MARGINS: MarginPalette[] = [
  { bg: "#0b0d10", fg: "#e6e9ef", soft: "#b4bdcb", muted: "#8f9aad", accent: "#5aa9ff" },
  { bg: "#262c36", fg: "#e6e9ef", soft: "#b9c2d0", muted: "#9aa5b8", accent: "#6db3ff" },
  { bg: "#d9dee7", fg: "#14161a", soft: "#303847", muted: "#4a5466", accent: "#0a53b0" },
  { bg: "#ffffff", fg: "#14161a", soft: "#3b4351", muted: "#5b6577", accent: "#0b5cc2" },
];

/** The margin must be told apart from the canvas: at least this contrast between the two surfaces. */
const MARGIN_MIN_CONTRAST = 1.15;

/** The margin surface closest to the canvas colour that is not too close to it. */
export function pickMargin(canvas: string): MarginPalette {
  const apart = MARGINS.filter((m) => contrast(canvas, m.bg) >= MARGIN_MIN_CONTRAST);
  if (apart.length === 0) return MARGINS.reduce((a, b) => (contrast(canvas, a.bg) >= contrast(canvas, b.bg) ? a : b));
  return apart.reduce((a, b) => (contrast(canvas, a.bg) <= contrast(canvas, b.bg) ? a : b));
}

/** Set the theme variables on the page for a canvas of colour `bg`. */
export function applyTheme(bg: string, root: HTMLElement = document.documentElement) {
  const dark = readableOn(bg) === "#ffffff";
  const ink = dark ? "#e6e9ef" : "#14161a";
  const margin = pickMargin(bg);
  // Everything that is a surface (margin, panels, buttons, hint) uses the margin's palette.
  // The graph title is drawn straight on the canvas, so it is coloured for the canvas.
  const vars: Record<string, string> = {
    "--bg": bg,
    "--panel": margin.bg,
    "--fg": margin.fg,
    "--soft": margin.soft,
    "--muted": margin.muted,
    "--line": mix(margin.bg, margin.fg, 0.3),
    "--field": mix(margin.bg, margin.fg, 0.07),
    "--accent": margin.accent,
    "--danger": readableOn(margin.bg) === "#ffffff" ? "#ff8c8c" : "#b3261e",
    "--title-fg": ink,
    "--title-muted": mix(bg, ink, 0.62),
    "color-scheme": dark ? "dark" : "light",
  };
  for (const [k, v] of Object.entries(vars)) root.style.setProperty(k, v);
}
