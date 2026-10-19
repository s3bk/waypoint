// Drawing the shapes a type describes: node outlines (with solid, dashed or
// dotted borders) and edge lines with arrowheads at either end.

import type { Graphics } from "pixi.js";
import type { Vec2 } from "./graph";
import { type EdgeType, hexNum } from "./style";

function roundRect(w: number, h: number, r: number): Vec2[] {
  const pts: Vec2[] = [];
  const corners: [number, number, number][] = [
    [w - r, r, -Math.PI / 2],
    [w - r, h - r, 0],
    [r, h - r, Math.PI / 2],
    [r, r, Math.PI],
  ];
  for (const [cx, cy, start] of corners) {
    for (let i = 0; i <= 6; i++) {
      const a = start + (i / 6) * (Math.PI / 2);
      pts.push({ x: cx + Math.cos(a) * r, y: cy + Math.sin(a) * r });
    }
  }
  return pts;
}

/** `pts` with each corner replaced by a small arc of at most radius `r`. */
function roundPolygon(pts: Vec2[], r: number): Vec2[] {
  const out: Vec2[] = [];
  const n = pts.length;
  for (let i = 0; i < n; i++) {
    const [prev, p, next] = [pts[(i + n - 1) % n]!, pts[i]!, pts[(i + 1) % n]!];
    const [lp, ln] = [Math.hypot(prev.x - p.x, prev.y - p.y), Math.hypot(next.x - p.x, next.y - p.y)];
    const cut = Math.min(r, lp / 2, ln / 2);
    const from = { x: p.x + ((prev.x - p.x) / lp) * cut, y: p.y + ((prev.y - p.y) / lp) * cut };
    const to = { x: p.x + ((next.x - p.x) / ln) * cut, y: p.y + ((next.y - p.y) / ln) * cut };
    for (let k = 0; k <= 6; k++) {
      const t = k / 6;
      // Quadratic curve through the corner point.
      out.push({
        x: (1 - t) * (1 - t) * from.x + 2 * (1 - t) * t * p.x + t * t * to.x,
        y: (1 - t) * (1 - t) * from.y + 2 * (1 - t) * t * p.y + t * t * to.y,
      });
    }
  }
  return out;
}

/** Whether a node shape has sharp corners (so its border should be mitred, not rounded). The rhombus is always sharp. */
export function isSharp(shape: string, corners: string): boolean {
  if (shape === "rhombus") return true;
  if (shape === "pill" || shape === "ellipse") return false;
  return corners === "square";
}

/** The closed outline of a node shape in a `w` by `h` box; `corners` ("round" or "square") applies to rect and hexagon (a rhombus is always sharp). */
export function outline(shape: string, corners: string, w: number, h: number): Vec2[] {
  let pts: Vec2[];
  switch (shape) {
    case "pill":
      return roundRect(w, h, h / 2);
    case "ellipse":
      return Array.from({ length: 48 }, (_, i) => {
        const a = (i / 48) * Math.PI * 2;
        return { x: w / 2 + (Math.cos(a) * w) / 2, y: h / 2 + (Math.sin(a) * h) / 2 };
      });
    case "hexagon":
      pts = [{ x: h / 2, y: 0 }, { x: w - h / 2, y: 0 }, { x: w, y: h / 2 }, { x: w - h / 2, y: h }, { x: h / 2, y: h }, { x: 0, y: h / 2 }];
      break;
    case "rhombus":
      pts = [{ x: w / 2, y: 0 }, { x: w, y: h / 2 }, { x: w / 2, y: h }, { x: 0, y: h / 2 }];
      break;
    default:
      pts = [{ x: 0, y: 0 }, { x: w, y: 0 }, { x: w, y: h }, { x: 0, y: h }];
  }
  return isSharp(shape, corners) ? pts : roundPolygon(pts, 8);
}

/** Stroke a polyline (closed or not) as one path, solid, dashed or dotted. */
export function strokePath(g: Graphics, pts: Vec2[], closed: boolean, color: number, width: number, style: string, alpha = 1, join: "round" | "miter" = "round") {
  if (width <= 0 || pts.length < 2) return;
  const path = closed ? [...pts, pts[0]!] : pts;
  if (style === "solid") {
    g.poly(closed ? pts : path, closed).stroke({ width, color, alpha, join, cap: join === "miter" ? "butt" : "round", miterLimit: 10 });
    return;
  }
  // Dotted: round dots one width long with two widths between; dashed: longer strokes.
  const dash = style === "dotted" ? 0.01 : Math.max(6, width * 4);
  const gap = style === "dotted" ? Math.max(4, width * 2.2) : Math.max(4, width * 2.5);
  let on = true;
  let left = dash;
  for (let i = 0; i + 1 < path.length; i++) {
    const [a, b] = [path[i]!, path[i + 1]!];
    const len = Math.hypot(b.x - a.x, b.y - a.y);
    const [ux, uy] = len ? [(b.x - a.x) / len, (b.y - a.y) / len] : [0, 0];
    let d = 0;
    while (d < len) {
      const step = Math.min(left, len - d);
      if (on) g.moveTo(a.x + ux * d, a.y + uy * d).lineTo(a.x + ux * (d + step), a.y + uy * (d + step));
      d += step;
      left -= step;
      if (left <= 1e-9) {
        on = !on;
        left = on ? dash : gap;
      }
    }
  }
  g.stroke({ width, color, alpha, cap: "round" });
}

/** Where the ray from `center` toward `target` leaves the closed outline `pts` (a convex shape around `center`). */
export function exitPoint(pts: Vec2[], center: Vec2, target: Vec2): Vec2 {
  const [dx, dy] = [target.x - center.x, target.y - center.y];
  if (dx === 0 && dy === 0) return center;
  let best = Infinity;
  for (let i = 0; i < pts.length; i++) {
    const [a, b] = [pts[i]!, pts[(i + 1) % pts.length]!];
    const [ex, ey] = [b.x - a.x, b.y - a.y];
    const den = dx * ey - dy * ex;
    if (Math.abs(den) < 1e-9) continue; // parallel to this side
    const [rx, ry] = [a.x - center.x, a.y - center.y];
    const t = (rx * ey - ry * ex) / den; // along the ray
    const u = (rx * dy - ry * dx) / den; // along the side
    if (t > 1e-9 && u >= -1e-9 && u <= 1 + 1e-9 && t < best) best = t;
  }
  return best === Infinity ? center : { x: center.x + dx * best, y: center.y + dy * best };
}

/** How far the line stops short of its end so the head sits on the node edge. */
export function inset(kind: string, width: number): number {
  if (kind === "arrow") return 8 + width * 1.5;
  if (kind === "dot") return 2 * (3 + width * 0.8);
  return 0;
}

function head(g: Graphics, kind: string, tip: Vec2, toward: Vec2, color: number, width: number) {
  // `toward` is the unit vector from the tip along the line.
  const [ux, uy] = [toward.x, toward.y];
  if (kind === "arrow") {
    const len = inset(kind, width);
    const half = len * 0.5;
    const base = { x: tip.x + ux * len, y: tip.y + uy * len };
    g.poly([tip.x, tip.y, base.x - uy * half, base.y + ux * half, base.x + uy * half, base.y - ux * half]).fill(color);
  } else if (kind === "dot") {
    const r = 3 + width * 0.8;
    g.circle(tip.x + ux * r, tip.y + uy * r, r).fill(color);
  } else if (kind === "bar") {
    const half = 5 + width;
    g.moveTo(tip.x - uy * half, tip.y + ux * half).lineTo(tip.x + uy * half, tip.y - ux * half).stroke({ width: Math.max(2, width), color, cap: "butt" });
  }
}

/** An edge from `a` (the source's end) to `b` in the style of `t`, with `color` and `width` overriding it (selection, conflicts). */
export function drawEdge(
  g: Graphics,
  a: Vec2,
  b: Vec2,
  t: Pick<EdgeType, "line_style" | "start" | "end" | "width" | "color">,
  over: { color?: number; width?: number; style?: string; end?: string; start?: string } = {},
) {
  const color = over.color ?? hexNum(t.color);
  const width = over.width ?? t.width;
  const [start, end] = [over.start ?? t.start, over.end ?? t.end];
  const len = Math.hypot(b.x - a.x, b.y - a.y) || 1;
  const u = { x: (b.x - a.x) / len, y: (b.y - a.y) / len };
  const [ia, ib] = [inset(start, width), inset(end, width)];
  const from = { x: a.x + u.x * ia, y: a.y + u.y * ia };
  const to = { x: b.x - u.x * ib, y: b.y - u.y * ib };
  if (ia + ib < len) strokePath(g, [from, to], false, color, width, over.style ?? t.line_style);
  head(g, start, a, u, color, width);
  head(g, end, b, { x: -u.x, y: -u.y }, color, width);
}
