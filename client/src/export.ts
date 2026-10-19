// Export the graph as an SVG or PNG file, cropped to what is actually on the
// canvas (the nodes and links, plus a margin), not to the window or the camera.
// The SVG is built from the same scene the renderer draws; the PNG is that SVG
// rasterized by the browser.

import { inset, isSharp, outline } from "./draw";
import { readableOn } from "./style";
import { NODE_H, NODE_W, type Scene, type SceneEdge, type SceneNode } from "./scene";
import { LINE_HEIGHT } from "./wrap";

const FONT = "system-ui, -apple-system, 'Segoe UI', Roboto, sans-serif";
const CONFLICT = "#e5484d";
const MARGIN = 20;
/** Browsers refuse canvases much beyond this. */
const MAX_PNG_SIDE = 8192;
const MAX_PNG_PIXELS = 64_000_000;

const esc = (s: string) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const num = (n: number) => String(Math.round(n * 100) / 100);

/** The rectangle the scene occupies: nodes with their borders and badges, link ends with their heads. */
function bounds(scene: Scene) {
  let [x0, y0, x1, y1] = [Infinity, Infinity, -Infinity, -Infinity];
  const grow = (x: number, y: number, r = 0) => {
    x0 = Math.min(x0, x - r);
    y0 = Math.min(y0, y - r);
    x1 = Math.max(x1, x + r);
    y1 = Math.max(y1, y + r);
  };
  for (const n of scene.nodes) {
    const r = n.type.border_width * 1.5; // a mitred corner reaches further than the half width
    grow(n.x, n.y, r);
    grow(n.x + NODE_W, n.y + NODE_H + (n.key !== undefined ? 14 : 0), r);
  }
  for (const e of scene.edges) {
    const r = 8 + e.type.width * 1.5;
    grow(e.a.x, e.a.y, r);
    grow(e.b.x, e.b.y, r);
    if (e.label) grow(e.label.x, e.label.y, 14);
  }
  return { x: x0 - MARGIN, y: y0 - MARGIN, w: x1 - x0 + 2 * MARGIN, h: y1 - y0 + 2 * MARGIN };
}

function dashAttr(style: string, width: number): string {
  if (style === "dashed") return ` stroke-dasharray="${num(Math.max(6, width * 4))} ${num(Math.max(4, width * 2.5))}"`;
  if (style === "dotted") return ` stroke-dasharray="0.01 ${num(Math.max(4, width * 2.2))}"`;
  return "";
}

/** One link, mirroring `drawEdge`: the line stops short of its heads, which are drawn on the node edges. */
function edgeSvg(e: SceneEdge): string {
  const t = e.type;
  const color = e.conflicted ? CONFLICT : t.color;
  const style = e.conflicted ? "dashed" : t.line_style;
  const width = t.width;
  const len = Math.hypot(e.b.x - e.a.x, e.b.y - e.a.y) || 1;
  const u = { x: (e.b.x - e.a.x) / len, y: (e.b.y - e.a.y) / len };
  const [ia, ib] = [inset(t.start, width), inset(t.end, width)];
  let out = "";
  if (ia + ib < len) {
    const [x1, y1] = [e.a.x + u.x * ia, e.a.y + u.y * ia];
    const [x2, y2] = [e.b.x - u.x * ib, e.b.y - u.y * ib];
    out += `<line x1="${num(x1)}" y1="${num(y1)}" x2="${num(x2)}" y2="${num(y2)}" stroke="${color}" stroke-width="${width}" stroke-linecap="round"${dashAttr(style, width)}/>`;
  }
  const head = (kind: string, tip: { x: number; y: number }, ux: number, uy: number) => {
    if (kind === "arrow") {
      const l = inset(kind, width);
      const half = l * 0.5;
      const base = { x: tip.x + ux * l, y: tip.y + uy * l };
      return `<polygon points="${num(tip.x)},${num(tip.y)} ${num(base.x - uy * half)},${num(base.y + ux * half)} ${num(base.x + uy * half)},${num(base.y - ux * half)}" fill="${color}"/>`;
    }
    if (kind === "dot") {
      const r = 3 + width * 0.8;
      return `<circle cx="${num(tip.x + ux * r)}" cy="${num(tip.y + uy * r)}" r="${num(r)}" fill="${color}"/>`;
    }
    if (kind === "bar") {
      const half = 5 + width;
      return `<line x1="${num(tip.x - uy * half)}" y1="${num(tip.y + ux * half)}" x2="${num(tip.x + uy * half)}" y2="${num(tip.y - ux * half)}" stroke="${color}" stroke-width="${Math.max(2, width)}"/>`;
    }
    return "";
  };
  out += head(t.start, e.a, u.x, u.y) + head(t.end, e.b, -u.x, -u.y);
  if (e.label && t.label) {
    const deg = (e.label.angle * 180) / Math.PI;
    out += `<text transform="translate(${num(e.label.x)} ${num(e.label.y)}) rotate(${num(deg)})" text-anchor="middle" dy="0.35em" font-size="11" fill="${e.conflicted ? CONFLICT : t.text_color}">${esc(t.label)}</text>`;
  }
  return out;
}

/** `ink` is the text colour for what is drawn straight on the canvas. */
function nodeSvg(n: SceneNode, ink: string): string {
  const t = n.type;
  const pts = outline(t.shape, t.corners, NODE_W, NODE_H);
  const d = pts.map((p) => `${num(p.x)},${num(p.y)}`).join(" ");
  const stroke = t.border_width > 0 ? ` stroke="${t.border}" stroke-width="${t.border_width}" stroke-linejoin="${isSharp(t.shape, t.corners) ? "miter" : "round"}"` : "";
  let out = `<g transform="translate(${num(n.x)} ${num(n.y)})"><polygon points="${d}" fill="${t.bg}"${stroke}/>`;
  const lines = n.untitled ? ["untitled"] : n.lines;
  const cx = NODE_W / 2;
  const text = lines
    .map((l, i) => {
      // Baseline: the middle of this line's box, plus about a third of the font size.
      const y = NODE_H / 2 + (i - (lines.length - 1) / 2) * LINE_HEIGHT + 5;
      return `<tspan x="${cx}" y="${num(y)}">${esc(l)}</tspan>`;
    })
    .join("");
  out += `<text text-anchor="middle" font-size="14" fill="${t.fg}"${n.untitled ? ' fill-opacity="0.4"' : ""}>${text}</text>`;
  if (n.key !== undefined) {
    // Just below the node, in the canvas's text colour (see the renderer).
    out += `<text x="${NODE_W}" y="${NODE_H + 12}" text-anchor="end" font-size="10" fill="${ink}" fill-opacity="0.7">#${n.key}</text>`;
  }
  return out + "</g>";
}

/** The scene as an SVG document and its size in pixels, or null when there is nothing to export. */
export function sceneToSvg(scene: Scene): { svg: string; width: number; height: number } | null {
  if (scene.nodes.length === 0) return null;
  const b = bounds(scene);
  const [width, height] = [Math.ceil(b.w), Math.ceil(b.h)];
  const ink = readableOn(scene.background); // for text drawn straight on the canvas
  const body = scene.edges.map(edgeSvg).join("") + scene.nodes.map((n) => nodeSvg(n, ink)).join("");
  const svg =
    `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="${num(b.x)} ${num(b.y)} ${width} ${height}" font-family="${FONT}">` +
    `<rect x="${num(b.x)}" y="${num(b.y)}" width="${width}" height="${height}" fill="${scene.background}"/>` +
    body +
    `</svg>`;
  return { svg, width, height };
}

function download(blob: Blob, name: string) {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

/** A file name from the graph's title: letters and digits, dashes between. */
export function fileName(title: string, ext: string): string {
  const base = title.normalize("NFKD").replace(/[^\p{L}\p{N}]+/gu, "-").replace(/^-+|-+$/g, "").slice(0, 60);
  return `${base || "graph"}.${ext}`;
}

/** Save the scene as an SVG file. Returns false when the graph is empty. */
export function exportSvg(scene: Scene, name: string): boolean {
  const out = sceneToSvg(scene);
  if (!out) return false;
  download(new Blob([`<?xml version="1.0" encoding="UTF-8"?>\n${out.svg}`], { type: "image/svg+xml" }), name);
  return true;
}

/** Save the scene as a PNG file, at twice the size (less if that would be too large for a canvas). Returns false when the graph is empty. */
export async function exportPng(scene: Scene, name: string): Promise<boolean> {
  const out = sceneToSvg(scene);
  if (!out) return false;
  const scale = Math.min(2, MAX_PNG_SIDE / Math.max(out.width, out.height), Math.sqrt(MAX_PNG_PIXELS / (out.width * out.height)));
  const url = URL.createObjectURL(new Blob([out.svg], { type: "image/svg+xml" }));
  try {
    const img = new Image();
    img.width = out.width;
    img.height = out.height;
    await new Promise<void>((resolve, reject) => {
      img.onload = () => resolve();
      img.onerror = () => reject(new Error("the browser could not draw the graph"));
      img.src = url;
    });
    const canvas = document.createElement("canvas");
    canvas.width = Math.max(1, Math.floor(out.width * scale));
    canvas.height = Math.max(1, Math.floor(out.height * scale));
    const ctx = canvas.getContext("2d")!;
    ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
    const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/png"));
    if (!blob) throw new Error("the browser could not encode the PNG");
    download(blob, name);
    return true;
  } finally {
    URL.revokeObjectURL(url);
  }
}
