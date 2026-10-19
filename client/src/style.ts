// Node types and edge types: the registries a graph carries in its document
// (`node_types`, `edge_types`), with the same rules the server enforces
// (crates/model/src/style.rs). The server validates every change; this file
// reads whatever is there and falls back to defaults for anything unusable, and
// checks contrast so the editor can tell you before you save.

export const DEFAULT_NODE_TYPE = "00000000-0000-7000-8000-000000000101";
export const DEFAULT_EDGE_TYPE = "00000000-0000-7000-8000-000000000001";
export const DEFAULT_BACKGROUND = "#14161a";

export const MIN_CONTRAST = 4.5;
export const MIN_LINE_CONTRAST = 3;
export const MIN_EDGE_WIDTH = 1;
export const MAX_WIDTH = 10;

export const NODE_SHAPES = ["rect", "pill", "ellipse", "hexagon", "rhombus"] as const;
export const CORNERS = ["round", "square"] as const;
export const ARROWS = ["none", "arrow", "dot", "bar"] as const;
export const LINE_STYLES = ["solid", "dashed", "dotted"] as const;

export interface NodeType {
  id: string;
  name: string;
  shape: string;
  bg: string;
  fg: string;
  border: string;
  border_width: number;
  corners: string;
}

export interface EdgeType {
  id: string;
  name: string;
  label: string;
  color: string;
  width: number;
  line_style: string;
  start: string;
  end: string;
  text_color: string;
}

export const BUILTIN_NODE: NodeType = {
  id: DEFAULT_NODE_TYPE,
  name: "default",
  shape: "rect",
  bg: "#2b3340",
  fg: "#e6e9ef",
  border: "#4a5568",
  border_width: 1,
  corners: "round",
};

export const BUILTIN_EDGE: EdgeType = {
  id: DEFAULT_EDGE_TYPE,
  name: "link",
  label: "link",
  color: "#8aa0bd",
  width: 2,
  line_style: "solid",
  start: "none",
  end: "arrow",
  text_color: "#8793a8",
};

/** `#rrggbb` as lowercase, or null if it is not a colour. */
export function normalizeColor(s: unknown): string | null {
  if (typeof s !== "string") return null;
  const m = /^#([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(s.trim());
  if (!m) return null;
  const h = m[1]!.toLowerCase();
  return "#" + (h.length === 3 ? [...h].map((c) => c + c).join("") : h);
}

export function hexNum(color: string): number {
  return parseInt(color.slice(1), 16);
}

function luminance(color: string): number {
  const lin = (c: number) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const v = (i: number) => lin(parseInt(color.slice(1 + 2 * i, 3 + 2 * i), 16) / 255);
  return 0.2126 * v(0) + 0.7152 * v(1) + 0.0722 * v(2);
}

/** WCAG contrast ratio of two `#rrggbb` colours, 1 to 21. */
export function contrast(a: string, b: string): number {
  const [la, lb] = [luminance(a), luminance(b)];
  return (Math.max(la, lb) + 0.05) / (Math.min(la, lb) + 0.05);
}

/** Black or white, whichever reads better on `bg`. */
export function readableOn(bg: string): string {
  return contrast(bg, "#ffffff") >= contrast(bg, "#000000") ? "#ffffff" : "#000000";
}

export function clampEdgeWidth(w: number): number {
  return Math.min(MAX_WIDTH, Math.max(MIN_EDGE_WIDTH, Math.round(w) || MIN_EDGE_WIDTH));
}

export function clampBorderWidth(w: number): number {
  return Math.min(MAX_WIDTH, Math.max(0, Math.round(w) || 0));
}

type Fields = Map<string, unknown>;

const str = (f: Fields, k: string, fallback: string) => (typeof f.get(k) === "string" ? (f.get(k) as string) : fallback);
const color = (f: Fields, k: string, fallback: string) => normalizeColor(f.get(k)) ?? fallback;
const oneOf = (f: Fields, k: string, allowed: readonly string[], fallback: string) => {
  const v = f.get(k);
  return typeof v === "string" && allowed.includes(v) ? v : fallback;
};
const num = (f: Fields, k: string, fallback: number) => (typeof f.get(k) === "number" ? (f.get(k) as number) : fallback);

/** Keep the first type of each name (the earlier id), as the server does. */
function uniqueNames<T extends { id: string; name: string }>(defs: Map<string, T>): T[] {
  const seen = new Set<string>();
  return [...defs.values()]
    .sort((a, b) => (a.id < b.id ? -1 : 1))
    .filter((d) => (seen.has(d.name) ? false : (seen.add(d.name), true)));
}

/** Registry entries from the document: `id -> fields`, removed ones left out (the built-in is never removed). */
function entries(raw: Iterable<[string, Fields]>, builtin: string): [string, Fields][] {
  return [...raw].filter(([id, f]) => f.get("removed") !== true || id === builtin);
}

export function readNodeTypes(raw: Iterable<[string, Fields]>): NodeType[] {
  const defs = new Map<string, NodeType>([[BUILTIN_NODE.id, BUILTIN_NODE]]);
  for (const [id, f] of entries(raw, DEFAULT_NODE_TYPE)) {
    const base = defs.get(id) ?? { ...BUILTIN_NODE, id, name: id };
    const name = str(f, "name", base.name);
    const def: NodeType = {
      id,
      name,
      shape: oneOf(f, "shape", NODE_SHAPES, base.shape),
      bg: color(f, "bg", base.bg),
      fg: color(f, "fg", base.fg),
      border: color(f, "border", base.border),
      border_width: clampBorderWidth(num(f, "border_width", base.border_width)),
      corners: oneOf(f, "corners", CORNERS, base.corners),
    };
    if (f.get("shape") === "rounded") {
      // Written before corners existed.
      def.shape = "rect";
      if (f.get("corners") === undefined) def.corners = "round";
    }
    if (contrast(def.bg, def.fg) < MIN_CONTRAST) [def.bg, def.fg] = [BUILTIN_NODE.bg, BUILTIN_NODE.fg];
    defs.set(id, def);
  }
  return uniqueNames(defs);
}

export function readEdgeTypes(raw: Iterable<[string, Fields]>, background: string): EdgeType[] {
  const defs = new Map<string, EdgeType>([[BUILTIN_EDGE.id, BUILTIN_EDGE]]);
  for (const [id, f] of entries(raw, DEFAULT_EDGE_TYPE)) {
    const known = defs.has(id);
    const base = defs.get(id) ?? { ...BUILTIN_EDGE, id, name: id };
    const name = str(f, "name", base.name);
    defs.set(id, {
      id,
      name,
      label: typeof f.get("label") === "string" ? (f.get("label") as string) : known ? base.label : name,
      color: color(f, "color", base.color),
      width: clampEdgeWidth(num(f, "width", base.width)),
      line_style: oneOf(f, "line_style", LINE_STYLES, base.line_style),
      start: oneOf(f, "start", ARROWS, base.start),
      end: oneOf(f, "end", ARROWS, base.end),
      text_color: color(f, "text_color", base.text_color),
    });
  }
  const readable = readableOn(background);
  for (const d of defs.values()) {
    if (contrast(background, d.color) < MIN_LINE_CONTRAST) d.color = readable;
    if (contrast(background, d.text_color) < MIN_CONTRAST) d.text_color = readable;
  }
  return uniqueNames(defs);
}
