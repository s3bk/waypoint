//! Tidy: lay out a whole graph from scratch (port of `tools/layout-experiments`,
//! findings in LAYOUT.md).
//!
//! 1. A clustered force-directed drawing: nodes of one type are pulled to a cell of a
//!    grid (cells ordered by how far right the type sits), several seeds, the one with
//!    the shortest links and fewest backward links wins.
//! 2. One annealing search over single-node moves, with one objective: 1000 per defect
//!    plus link length and distance to the centre. A defect is any of
//!    - boxes closer than `GAP`, or a linked pair without room for its label,
//!    - a link through the core (middle third) of an unrelated node,
//!    - an edge label under a node,
//!    - two edge labels overlapping.
//! 3. A mild stretch toward the wanted aspect ratio, kept only if it adds no defect.
//!
//! Deterministic: fixed seeds, no clock. Positions are top-left corners with the
//! drawing's top-left at the origin. Above `MAX_NODES` the layered drawing is used.

use std::collections::{BTreeMap, BTreeSet};

use waypoint_model::{NODE_HEIGHT, NODE_WIDTH, NodeId, Vec2};

use crate::layered;

/// More nodes than this get the layered drawing (the force step is quadratic).
pub const MAX_NODES: usize = 800;
/// Width of the text of an edge label at the client's 11 px font, per character.
pub const LABEL_CHAR_WIDTH: f64 = 5.8;

const W: f64 = NODE_WIDTH as f64;
const H: f64 = NODE_HEIGHT as f64;
/// Least space between boxes.
const GAP: f64 = 28.0;
/// Matches `ARROW_LEN` in the client: a label needs `width + 2 * ARROW_LEN + 8` of visible link.
const ARROW_LEN: f64 = 10.0;
/// A link may cut a third into a node: only its middle third is an obstacle.
const CORE: [f64; 2] = [W / 6.0, H / 6.0];
const SEEDS: u64 = 4;
const FORCE_SEEDS: u64 = 6;

type P2 = [f64; 2];

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// In `[0, 1)`.
    fn f(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.f()
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// An edge label as an oriented rectangle.
#[derive(Clone, Copy)]
struct LabelRect {
    c: P2,
    d: P2,
    half_len: f64,
    half_h: f64,
}

struct Ctx {
    links: Vec<(usize, usize)>,
    /// Link indices touching each node.
    inc: Vec<Vec<usize>>,
    /// Visible link length a node pair needs between its boxes (0 unless they are linked and the link has a label).
    pair_need: Vec<f64>,
    n: usize,
    /// Text width of each link's label, 0 for a link without one.
    lw: Vec<f64>,
    /// Visible length each link needs for its label.
    need: Vec<f64>,
    /// Size of the area the drawing must stay inside (node boxes included), if any.
    bounds: Option<P2>,
}

fn box_exit(c: P2, t: P2) -> P2 {
    let (dx, dy) = (t[0] - c[0], t[1] - c[1]);
    if dx == 0.0 && dy == 0.0 {
        return c;
    }
    let kx = if dx == 0.0 { f64::INFINITY } else { (W / 2.0) / dx.abs() };
    let ky = if dy == 0.0 { f64::INFINITY } else { (H / 2.0) / dy.abs() };
    let k = kx.min(ky);
    [c[0] + dx * k, c[1] + dy * k]
}

/// Length of the link between the two boxes, 0 when they touch or overlap.
fn visible(a: P2, b: P2) -> f64 {
    let (ea, eb) = (box_exit(a, b), box_exit(b, a));
    if (eb[0] - ea[0]) * (b[0] - a[0]) + (eb[1] - ea[1]) * (b[1] - a[1]) < 0.0 {
        return 0.0;
    }
    (eb[0] - ea[0]).hypot(eb[1] - ea[1])
}

/// Does the segment `a`-`b` cross the box centred `c` with half sizes `hw`, `hh`?
fn seg_hits(a: P2, b: P2, c: P2, hw: f64, hh: f64) -> bool {
    let (x0, y0) = (a[0] - c[0], a[1] - c[1]);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, x0 + hw), (dx, hw - x0), (-dy, y0 + hh), (dy, hh - y0)] {
        if p.abs() < 1e-9 {
            if q < 0.0 {
                return false;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                if r > t1 {
                    return false;
                }
                t0 = t0.max(r);
            } else {
                if r < t0 {
                    return false;
                }
                t1 = t1.min(r);
            }
        }
    }
    t0 <= t1
}

/// The label of a link between centres `a` and `b`, placed as the client draws it:
/// at the middle of the visible link, 8 px to the upright text's up side.
fn label_rect(a: P2, b: P2, lw: f64) -> LabelRect {
    let (ea, eb) = (box_exit(a, b), box_exit(b, a));
    let mid = [(ea[0] + eb[0]) / 2.0, (ea[1] + eb[1]) / 2.0];
    let (mut dx, mut dy) = (eb[0] - ea[0], eb[1] - ea[1]);
    let l = dx.hypot(dy);
    if l > 0.0 {
        dx /= l;
        dy /= l;
    } else {
        dx = 1.0;
        dy = 0.0;
    }
    if dx < 0.0 || (dx.abs() < 1e-9 && dy < 0.0) {
        dx = -dx;
        dy = -dy;
    }
    let (nx, ny) = (dy, -dx);
    LabelRect { c: [mid[0] + nx * 8.0, mid[1] + ny * 8.0], d: [dx, dy], half_len: lw / 2.0 + 3.0, half_h: 7.5 }
}

/// Is a point of the label's text box inside the node centred `c`? (7 x 3 samples.)
fn label_covered(r: &LabelRect, c: P2, pad: f64) -> bool {
    let (dx, dy) = (r.d[0], r.d[1]);
    let (nx, ny) = (dy, -dx);
    let hl = r.half_len - 3.0;
    for s in [-1.0, -0.66, -0.33, 0.0, 0.33, 0.66, 1.0] {
        for h in [-6.5, 0.0, 6.5] {
            let x = r.c[0] + dx * s * hl + nx * h;
            let y = r.c[1] + dy * s * hl + ny * h;
            if (x - c[0]).abs() < W / 2.0 + pad && (y - c[1]).abs() < H / 2.0 + pad {
                return true;
            }
        }
    }
    false
}

/// Separating-axis test for two oriented rectangles.
fn rects_overlap(a: &LabelRect, b: &LabelRect) -> bool {
    let proj = |r: &LabelRect, ax: P2| {
        r.half_len * (ax[0] * r.d[0] + ax[1] * r.d[1]).abs() + r.half_h * (ax[0] * -r.d[1] + ax[1] * r.d[0]).abs()
    };
    for r in [a, b] {
        for ax in [r.d, [-r.d[1], r.d[0]]] {
            let dist = ((b.c[0] - a.c[0]) * ax[0] + (b.c[1] - a.c[1]) * ax[1]).abs();
            if dist > proj(a, ax) + proj(b, ax) {
                return false;
            }
        }
    }
    true
}

impl Ctx {
    /// `lw[k]` is the text width of link `k`'s label in px, 0 if it has none.
    fn new(n: usize, links: Vec<(usize, usize)>, lw: Vec<f64>) -> Self {
        let mut inc = vec![Vec::new(); n];
        let mut pair_need = vec![0.0f64; n * n];
        let need: Vec<f64> = lw.iter().map(|&w| if w > 0.0 { w + 2.0 * ARROW_LEN + 8.0 + 6.0 } else { 0.0 }).collect();
        for (k, &(f, t)) in links.iter().enumerate() {
            inc[f].push(k);
            inc[t].push(k);
            pair_need[f * n + t] = pair_need[f * n + t].max(need[k]);
            pair_need[t * n + f] = pair_need[t * n + f].max(need[k]);
        }
        Ctx { links, inc, pair_need, n, lw, need, bounds: None }
    }

    /// Keep a box centre inside the bounds, if there are any.
    fn clamp(&self, q: &mut P2) {
        if let Some([bw, bh]) = self.bounds {
            q[0] = q[0].clamp(W / 2.0, (bw - W / 2.0).max(W / 2.0));
            q[1] = q[1].clamp(H / 2.0, (bh - H / 2.0).max(H / 2.0));
        }
    }

    fn pair_ok(&self, i: usize, j: usize, a: P2, b: P2) -> bool {
        if (a[0] - b[0]).abs() < W + GAP && (a[1] - b[1]).abs() < H + GAP {
            return false;
        }
        let need = self.pair_need[i * self.n + j];
        need == 0.0 || visible(a, b) >= need
    }

    fn label(&self, p: &[P2], k: usize) -> LabelRect {
        let (f, t) = self.links[k];
        label_rect(p[f], p[t], self.lw[k])
    }

    /// Defects that involve node `i` if it stood at `q` (others at `p`).
    fn defects(&self, p: &[P2], i: usize, q: P2) -> usize {
        let at = |j: usize| if j == i { q } else { p[j] };
        let mut c = 0;
        for j in 0..self.n {
            if j != i && !self.pair_ok(i, j, q, p[j]) {
                c += 1;
            }
        }
        for (k, &(f, t)) in self.links.iter().enumerate() {
            let incident = f == i || t == i;
            if !incident {
                if seg_hits(p[f], p[t], q, CORE[0], CORE[1]) {
                    c += 1;
                }
            } else {
                for j in 0..self.n {
                    if j != f && j != t && seg_hits(at(f), at(t), p[j], CORE[0], CORE[1]) {
                        c += 1;
                    }
                }
            }
            if self.lw[k] > 0.0 && !incident && label_covered(&label_rect(p[f], p[t], self.lw[k]), q, 2.0) {
                c += 1;
            }
        }
        {
            let own: Vec<(usize, LabelRect)> = self.inc[i]
                .iter()
                .filter(|&&k| self.lw[k] > 0.0)
                .map(|&k| {
                    let (f, t) = self.links[k];
                    (k, label_rect(at(f), at(t), self.lw[k]))
                })
                .collect();
            for &(k, r) in &own {
                let (f, t) = self.links[k];
                for j in 0..self.n {
                    if j != f && j != t && label_covered(&r, p[j], 2.0) {
                        c += 1;
                    }
                }
                for k2 in 0..self.links.len() {
                    if k2 == k || self.lw[k2] <= 0.0 {
                        continue;
                    }
                    if let Some(&(_, r2)) = own.iter().find(|(o, _)| *o == k2) {
                        if k2 < k {
                            continue;
                        }
                        if rects_overlap(&r, &r2) {
                            c += 1;
                        }
                    } else if rects_overlap(&r, &self.label(p, k2)) {
                        c += 1;
                    }
                }
            }
        }
        c
    }

    /// All defects, and the nodes that take part in one.
    fn scan(&self, p: &[P2]) -> (usize, Vec<usize>) {
        let mut count = 0;
        let mut bad = Vec::new();
        for i in 0..self.n {
            for j in i + 1..self.n {
                if !self.pair_ok(i, j, p[i], p[j]) {
                    count += 1;
                    bad.extend([i, j]);
                }
            }
        }
        for &(f, t) in &self.links {
            for j in 0..self.n {
                if j != f && j != t && seg_hits(p[f], p[t], p[j], CORE[0], CORE[1]) {
                    count += 1;
                    bad.extend([j, f, t]);
                }
            }
        }
        {
            let rects: Vec<Option<LabelRect>> = (0..self.links.len()).map(|k| (self.lw[k] > 0.0).then(|| self.label(p, k))).collect();
            for (k, &(f, t)) in self.links.iter().enumerate() {
                let Some(r) = rects[k] else { continue };
                for j in 0..self.n {
                    if j != f && j != t && label_covered(&r, p[j], 0.0) {
                        count += 1;
                        bad.extend([f, t, j]);
                    }
                }
                for k2 in k + 1..self.links.len() {
                    if rects[k2].is_some_and(|r2| rects_overlap(&r, &r2)) {
                        count += 1;
                        bad.extend([f, t, self.links[k2].0, self.links[k2].1]);
                    }
                }
            }
        }
        (count, bad)
    }

    fn link_len(&self, p: &[P2], k: usize) -> f64 {
        let (f, t) = self.links[k];
        (p[f][0] - p[t][0]).hypot(p[f][1] - p[t][1])
    }

    /// Push linked pairs apart along their line until their label fits between the boxes.
    fn separate_links(&self, p: &mut [P2]) -> bool {
        let mut moved = false;
        for (k, &(f, t)) in self.links.iter().enumerate() {
            let need = self.need[k];
            if need == 0.0 {
                continue;
            }
            let v = visible(p[f], p[t]);
            if v < need {
                let (dx, dy) = (p[t][0] - p[f][0], p[t][1] - p[f][1]);
                let l = dx.hypot(dy);
                let (ux, uy) = if l > 0.0 { (dx / l, dy / l) } else { (1.0, 0.0) };
                let push = (need - v) / 2.0 + 1.0;
                p[f][0] -= ux * push;
                p[f][1] -= uy * push;
                p[t][0] += ux * push;
                p[t][1] += uy * push;
                moved = true;
            }
        }
        moved
    }
}

fn extent(p: &[P2]) -> (f64, f64) {
    let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for q in p {
        x0 = x0.min(q[0]);
        x1 = x1.max(q[0]);
        y0 = y0.min(q[1]);
        y1 = y1.max(q[1]);
    }
    (x1 - x0 + W, y1 - y0 + H)
}

/// Layer of each node: one more than its deepest destination (cycles are cut after `n` rounds).
fn layers(n: usize, links: &[(usize, usize)]) -> Vec<usize> {
    let mut layer = vec![0usize; n];
    for _ in 0..n {
        let mut changed = false;
        for &(f, t) in links {
            if layer[f] < layer[t] + 1 {
                layer[f] = layer[t] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    layer
}

/// Clustered force-directed drawing, aspect stretched, boxes separated.
fn force(ctx: &Ctx, kinds: &[u32], layer: &[usize], seed: u64, aspect: f64) -> Vec<P2> {
    let n = ctx.n;
    let mut rng = Rng::new(seed);
    let mut order: Vec<u32> = kinds.iter().copied().collect::<BTreeSet<_>>().into_iter().collect();
    let mean = |t: u32| {
        let (s, c) = (0..n).filter(|&i| kinds[i] == t).fold((0.0, 0.0), |(s, c), i| (s + layer[i] as f64, c + 1.0));
        s / c
    };
    order.sort_by(|a, b| mean(*a).total_cmp(&mean(*b)));
    let kinds_n = order.len();
    let cols = ((kinds_n as f64 * aspect).sqrt().round() as usize).max(1);
    let rows = kinds_n.div_ceil(cols).max(1);
    // Inside bounds the cells share the area; otherwise they are a fixed size.
    let (cell, origin) = match ctx.bounds {
        Some([bw, bh]) => ([bw / cols as f64, bh / rows as f64], [0.5, 0.5]),
        None => ([360.0, 360.0 / aspect * 1.25], [0.0, 0.0]),
    };
    let centre: Vec<P2> = (0..n)
        .map(|i| {
            let c = order.iter().position(|&t| t == kinds[i]).unwrap_or(0);
            [((c % cols) as f64 + origin[0]) * cell[0], ((c / cols) as f64 + origin[1]) * cell[1]]
        })
        .collect();
    let pull = if kinds_n > 1 { 0.5 } else { 0.0 };
    // Spread grows with the node count so dense starts do not collapse.
    let (k, spread) = match ctx.bounds {
        // The repulsion reaches about as far as one node's share of the area.
        Some([bw, bh]) => ((bw * bh / n as f64).sqrt().clamp(60.0, 140.0), (cell[0].min(cell[1]) / 3.0).max(30.0)),
        None => (120.0 + 2.0 * (n as f64).sqrt(), 150.0 * (n as f64 / 36.0).sqrt().max(1.0)),
    };
    let mut p: Vec<P2> = centre.iter().map(|c| [c[0] + rng.range(-spread, spread), c[1] + rng.range(-spread, spread)]).collect();
    p.iter_mut().for_each(|q| ctx.clamp(q));
    let iters = 700;
    for it in 0..iters {
        let tt = 1.0 - it as f64 / iters as f64;
        let mut fo = vec![[0.0f64; 2]; n];
        for i in 0..n {
            for j in i + 1..n {
                let (dx, dy) = (p[i][0] - p[j][0], p[i][1] - p[j][1]);
                let d2 = dx * dx + (1.6 * dy) * (1.6 * dy) + 1e-6;
                let s = k * k / d2 * 0.6;
                fo[i][0] += dx * s;
                fo[i][1] += dy * s;
                fo[j][0] -= dx * s;
                fo[j][1] -= dy * s;
            }
        }
        for &(f, t) in &ctx.links {
            let (dx, dy) = (p[t][0] - p[f][0], p[t][1] - p[f][1]);
            let l = dx.hypot(dy) + 1e-6;
            let a = l / k * 0.5;
            fo[f][0] += dx * a;
            fo[f][1] += dy * a;
            fo[t][0] -= dx * a;
            fo[t][1] -= dy * a;
            // Dependants sit right of what they depend on (within a type).
            if kinds[f] == kinds[t] {
                let gap = (p[f][0] - p[t][0]) - (W + 70.0);
                if gap < 0.0 {
                    fo[f][0] -= gap * 0.3;
                    fo[t][0] += gap * 0.3;
                }
            }
        }
        let cap = (12.0 * tt + 0.5) * 8.0;
        for i in 0..n {
            fo[i][0] += (centre[i][0] - p[i][0]) * pull * (0.4 + tt);
            fo[i][1] += (centre[i][1] - p[i][1]) * pull * (0.4 + tt);
            let norm = fo[i][0].hypot(fo[i][1]) + 1e-6;
            let step = norm.min(cap) * 0.25 / norm;
            p[i][0] += fo[i][0] * step;
            p[i][1] += fo[i][1] * step;
            ctx.clamp(&mut p[i]);
        }
    }
    let (w, h) = extent(&p);
    if ctx.bounds.is_none() && w / h < aspect {
        let s = aspect * h / w;
        p.iter_mut().for_each(|q| q[0] *= s);
    }
    for _ in 0..400 {
        let mut moved = false;
        for i in 0..n {
            for j in i + 1..n {
                let (dx, dy) = (p[j][0] - p[i][0], p[j][1] - p[i][1]);
                let (ox, oy) = (W + GAP - dx.abs(), H + GAP - dy.abs());
                if ox > 0.0 && oy > 0.0 {
                    moved = true;
                    if ox / (W + GAP) < oy / (H + GAP) {
                        let s = ox / 2.0 * if dx >= 0.0 { 1.0 } else { -1.0 };
                        p[i][0] -= s;
                        p[j][0] += s;
                    } else {
                        let s = oy / 2.0 * if dy >= 0.0 { 1.0 } else { -1.0 };
                        p[i][1] -= s;
                        p[j][1] += s;
                    }
                }
            }
        }
        if ctx.separate_links(&mut p) {
            moved = true;
        }
        p.iter_mut().for_each(|q| ctx.clamp(q));
        if !moved {
            break;
        }
    }
    p
}

/// How good a drawing is as a start: short links, few backward ones.
fn force_score(ctx: &Ctx, p: &[P2]) -> f64 {
    if ctx.links.is_empty() {
        return 0.0;
    }
    let total: f64 = (0..ctx.links.len()).map(|k| ctx.link_len(p, k)).sum();
    let back = ctx.links.iter().filter(|&&(f, t)| p[f][0] < p[t][0]).count();
    total / ctx.links.len() as f64 + 250.0 * back as f64
}

/// Area of the drawing, weighted against a bad aspect ratio.
fn area_score(p: &[P2], aspect: f64) -> f64 {
    let (w, h) = extent(p);
    w * h * (1.0 + 0.5 * (w / h / aspect).ln().abs())
}

/// Annealing over single-node moves. Returns (defects, area score, positions) of the best state seen.
fn search(ctx: &Ctx, start: &[P2], seed: u64, steps: usize, aspect: f64) -> (usize, f64, Vec<P2>) {
    let n = ctx.n;
    let mut rng = Rng::new(seed);
    let mut p = start.to_vec();
    let mut best: (usize, f64, Vec<P2>) = (usize::MAX, f64::MAX, p.clone());
    let (mut bad, mut stuck) = (ctx.scan(&p).1, 0usize);
    let compact = |p: &[P2], i: usize, q: P2, c: P2| {
        let e: f64 = ctx.inc[i]
            .iter()
            .map(|&k| {
                let (f, t) = ctx.links[k];
                let (a, b) = (if f == i { q } else { p[f] }, if t == i { q } else { p[t] });
                (a[0] - b[0]).hypot(a[1] - b[1])
            })
            .sum();
        0.5 * e + 0.35 * (q[0] - c[0]).hypot((q[1] - c[1]) * 1.78)
    };
    for step in 0..steps {
        if step % 150 == 0 {
            let (count, b) = ctx.scan(&p);
            bad = b;
            let key = area_score(&p, aspect);
            if (count, key) < (best.0, best.1) {
                best = (count, key, p.clone());
                stuck = 0;
            } else {
                stuck += 1;
            }
        }
        let c = [p.iter().map(|q| q[0]).sum::<f64>() / n as f64, p.iter().map(|q| q[1]).sum::<f64>() / n as f64];
        let i = if !bad.is_empty() && rng.f() < 0.85 { bad[rng.below(bad.len())] } else { rng.below(n) };
        // Cycles of reheating; hotter and bigger moves when stuck.
        let phase = (step % 3000) as f64 / 3000.0;
        let t = (if best.0 > 0 { 60.0 + 40.0 * stuck.min(6) as f64 } else { 25.0 }) * (1.0 - phase) + 0.5;
        let radii: [f64; 6] = if stuck < 3 { [10.0, 20.0, 40.0, 70.0, 110.0, 180.0] } else { [20.0, 40.0, 70.0, 110.0, 180.0, 260.0] };
        let r = radii[rng.below(6)];
        let a = rng.range(0.0, std::f64::consts::TAU);
        let mut q = [p[i][0] + r * a.cos(), p[i][1] + r * a.sin()];
        // Whole pixels, so the result is exactly what was checked.
        q = [q[0].round(), q[1].round()];
        ctx.clamp(&mut q);
        let delta = 1000.0 * (ctx.defects(&p, i, q) as f64 - ctx.defects(&p, i, p[i]) as f64) + compact(&p, i, q, c) - compact(&p, i, p[i], c);
        if delta < 0.0 || rng.f() < (-delta / t).exp() {
            p[i] = q;
        }
    }
    let (count, _) = ctx.scan(&p);
    let key = area_score(&p, aspect);
    if (count, key) < (best.0, best.1) {
        best = (count, key, p);
    }
    best
}

/// Width in px of the text of each link's label (`(from, to)` to width); a link not listed has no label.
pub type Labels = BTreeMap<(NodeId, NodeId), f64>;

fn build_ctx(nodes: &[(NodeId, u32)], edges: &[(NodeId, NodeId)], labels: &Labels) -> Ctx {
    let index: BTreeMap<NodeId, usize> = nodes.iter().enumerate().map(|(i, (id, _))| (*id, i)).collect();
    let mut links: BTreeMap<(usize, usize), f64> = BTreeMap::new();
    for &(f, t) in edges {
        let (Some(&a), Some(&b)) = (index.get(&f), index.get(&t)) else { continue };
        if a != b {
            let w = labels.get(&(f, t)).copied().unwrap_or(0.0).max(0.0);
            let slot = links.entry((a, b)).or_insert(0.0);
            *slot = slot.max(w);
        }
    }
    Ctx::new(nodes.len(), links.keys().copied().collect(), links.values().copied().collect())
}

/// How many defects (see the module docs) a drawing has: nodes too close, links through nodes,
/// links too short for their label, labels under nodes or on each other. `positions` are top-left corners.
pub fn defect_count(
    nodes: &[(NodeId, u32)],
    edges: &[(NodeId, NodeId)],
    positions: &BTreeMap<NodeId, Vec2>,
    labels: &Labels,
) -> usize {
    let ctx = build_ctx(nodes, edges, labels);
    let centres: Vec<P2> =
        nodes.iter().map(|(id, _)| positions.get(id).map_or([0.0, 0.0], |v| [v.x as f64 + W / 2.0, v.y as f64 + H / 2.0])).collect();
    ctx.scan(&centres).0
}

/// How [`arrange`] lays a graph out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Force-directed, nodes of one type grouped together, then cleared of defects (see the module docs).
    Clustered,
    /// The same without grouping: types are ignored.
    Force,
    /// Left to right in layers, dependencies on the left ([`layered`]).
    Layered,
    /// Keep the arrangement as it is and only move nodes to clear defects and tighten the drawing.
    Polish,
}

impl Method {
    pub fn parse(name: &str) -> Option<Method> {
        Some(match name {
            "clustered" => Method::Clustered,
            "force" => Method::Force,
            "layered" => Method::Layered,
            "polish" => Method::Polish,
            _ => return None,
        })
    }
}

/// Top-left positions for every node, overwriting nothing: the caller decides what to do
/// with them. `nodes` pairs each node with its type (any number; equal numbers cluster),
/// `edges` are `(from, to)` pairs (pairs not both in `nodes` are ignored), `aspect` is the
/// wanted width / height of the drawing (16 / 9 for a screen), `labels` the label text width of the links that have one.
pub fn tidy(nodes: &[(NodeId, u32)], edges: &[(NodeId, NodeId)], aspect: f64, labels: &Labels) -> BTreeMap<NodeId, Vec2> {
    arrange(Method::Clustered, nodes, edges, &BTreeMap::new(), aspect, labels, None)
}

/// Positions (top-left corners, the drawing's top-left at the origin) for every node by `method`.
/// `current` holds where the nodes are now; only [`Method::Polish`] uses it. `bounds` is the width and
/// height the drawing must stay inside, e.g. the screen; the force-directed methods ([`Method::Clustered`],
/// [`Method::Force`]) honour it as well as they can (if the nodes cannot fit without overlapping, some
/// defects remain rather than the drawing growing). Other arguments as for [`tidy`].
pub fn arrange(
    method: Method,
    nodes: &[(NodeId, u32)],
    edges: &[(NodeId, NodeId)],
    current: &BTreeMap<NodeId, Vec2>,
    aspect: f64,
    labels: &Labels,
    bounds: Option<(f64, f64)>,
) -> BTreeMap<NodeId, Vec2> {
    let n = nodes.len();
    if n == 0 {
        return BTreeMap::new();
    }
    let ids: Vec<NodeId> = nodes.iter().map(|(id, _)| *id).collect();
    if n > MAX_NODES || method == Method::Layered {
        return layered(&ids, edges);
    }
    let aspect = if aspect.is_finite() && aspect > 0.2 && aspect < 5.0 { aspect } else { 16.0 / 9.0 };
    let mut ctx = build_ctx(nodes, edges, labels);
    if matches!(method, Method::Clustered | Method::Force) {
        ctx.bounds = bounds.filter(|(w, h)| w.is_finite() && h.is_finite() && *w >= W && *h >= H).map(|(w, h)| [w, h]);
    }

    let start: Vec<P2> = if method == Method::Polish {
        // Nodes without a known position go to the middle of the others.
        let known: Vec<P2> = ids.iter().filter_map(|id| current.get(id)).map(|v| [v.x as f64 + W / 2.0, v.y as f64 + H / 2.0]).collect();
        let mid = if known.is_empty() {
            [0.0, 0.0]
        } else {
            [known.iter().map(|q| q[0]).sum::<f64>() / known.len() as f64, known.iter().map(|q| q[1]).sum::<f64>() / known.len() as f64]
        };
        ids.iter().map(|id| current.get(id).map(|v| [v.x as f64 + W / 2.0, v.y as f64 + H / 2.0]).unwrap_or(mid)).collect()
    } else {
        // Clustered force-directed start, best of several seeds.
        let kinds: Vec<u32> = if method == Method::Force { vec![0; n] } else { nodes.iter().map(|(_, k)| *k).collect() };
        let layer = layers(n, &ctx.links);
        (1..=FORCE_SEEDS)
            .map(|s| force(&ctx, &kinds, &layer, s, aspect))
            .min_by(|a, b| force_score(&ctx, a).total_cmp(&force_score(&ctx, b)))
            .expect("at least one seed")
    };

    // One search to clear the defects, a second from a clean result to shrink it.
    let steps = (6000 + 60 * n).min(60_000);
    let mut cur = start;
    cur.iter_mut().for_each(|q| *q = [q[0].round(), q[1].round()]);
    // A tight area may need more rounds to clear; with room, a second round from a clean result shrinks it.
    for round in 0..3 {
        let best = (1..=SEEDS)
            .map(|s| search(&ctx, &cur, s + 10 * round, steps, aspect))
            .min_by(|a, b| (a.0, a.1).partial_cmp(&(b.0, b.1)).expect("finite scores"))
            .expect("at least one seed");
        let clean = best.0 == 0;
        cur = best.2;
        if clean && round >= 1 {
            break;
        }
    }

    // Stretch toward the aspect (at most 1.3x), kept only if no defect appears. A polish keeps the arrangement's shape.
    let (w, h) = extent(&cur);
    if method != Method::Polish && ctx.bounds.is_none() && w / h < aspect {
        let s = (aspect * h / w).min(1.3);
        let mut wide = cur.clone();
        wide.iter_mut().for_each(|q| q[0] = (q[0] * s).round());
        if ctx.scan(&wide).0 <= ctx.scan(&cur).0 {
            cur = wide;
        }
    }

    let (x0, y0) = cur.iter().fold((f64::MAX, f64::MAX), |(x, y), q| (x.min(q[0]), y.min(q[1])));
    ids.into_iter().zip(cur).map(|(id, q)| (id, Vec2::new((q[0] - x0).round() as i32, (q[1] - y0).round() as i32))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LABEL: f64 = 9.0 * LABEL_CHAR_WIDTH;

    /// Every link has a label of the same width.
    fn uniform(edges: &[(NodeId, NodeId)], width: f64) -> Labels {
        edges.iter().map(|&e| (e, width)).collect()
    }

    /// A graph with `n` nodes of 4 types, each linking to up to two earlier ones.
    fn graph(n: usize) -> (Vec<(NodeId, u32)>, Vec<(NodeId, NodeId)>) {
        let ids: Vec<NodeId> = (0..n).map(|_| NodeId::new()).collect();
        let nodes = ids.iter().enumerate().map(|(i, &id)| (id, (i % 4) as u32)).collect();
        let mut edges = Vec::new();
        for i in 1..n {
            edges.push((ids[i], ids[(i * 7) % i]));
            if i > 3 {
                edges.push((ids[i], ids[(i * 3 + 1) % i]));
            }
        }
        (nodes, edges)
    }

    fn centres(p: &BTreeMap<NodeId, Vec2>, nodes: &[(NodeId, u32)]) -> Vec<P2> {
        nodes.iter().map(|(id, _)| [p[id].x as f64 + W / 2.0, p[id].y as f64 + H / 2.0]).collect()
    }

    fn ctx_of(nodes: &[(NodeId, u32)], edges: &[(NodeId, NodeId)], lw: f64) -> Ctx {
        build_ctx(nodes, edges, &uniform(edges, lw))
    }

    #[test]
    fn every_method_is_clean_and_deterministic() {
        let (nodes, edges) = graph(14);
        let ctx = ctx_of(&nodes, &edges, LABEL);
        let current = tidy(&nodes, &edges, 16.0 / 9.0, &uniform(&edges, LABEL));
        for method in [Method::Clustered, Method::Force, Method::Polish] {
            let a = arrange(method, &nodes, &edges, &current, 16.0 / 9.0, &uniform(&edges, LABEL), None);
            assert_eq!(a, arrange(method, &nodes, &edges, &current, 16.0 / 9.0, &uniform(&edges, LABEL), None), "{method:?}");
            assert_eq!(ctx.scan(&centres(&a, &nodes)).0, 0, "{method:?} left defects");
        }
        let ids: Vec<NodeId> = nodes.iter().map(|(id, _)| *id).collect();
        assert_eq!(arrange(Method::Layered, &nodes, &edges, &current, 16.0 / 9.0, &uniform(&edges, LABEL), None), layered(&ids, &edges));
        assert_eq!(Method::parse("polish"), Some(Method::Polish));
        assert_eq!(Method::parse("nope"), None);
    }

    #[test]
    fn force_layouts_stay_inside_the_given_area() {
        let (nodes, edges) = graph(16);
        let (bw, bh) = (1500.0, 900.0);
        for method in [Method::Clustered, Method::Force] {
            let p = arrange(method, &nodes, &edges, &BTreeMap::new(), 16.0 / 9.0, &uniform(&edges, LABEL), Some((bw, bh)));
            for (id, v) in &p {
                assert!(v.x >= 0 && v.y >= 0 && v.x as f64 + W <= bw + 1.0 && v.y as f64 + H <= bh + 1.0, "{method:?}: {id:?} at {v:?} is outside {bw} x {bh}");
            }
            // Without a limit the same graph is free to spread.
            let free = arrange(method, &nodes, &edges, &BTreeMap::new(), 16.0 / 9.0, &uniform(&edges, LABEL), None);
            assert_ne!(p, free);
        }
    }

    #[test]
    fn polish_starts_from_the_current_arrangement() {
        let (nodes, edges) = graph(8);
        // Already clean: a polish keeps its shape, so nodes end up near where they were.
        let clean = tidy(&nodes, &edges, 16.0 / 9.0, &uniform(&edges, LABEL));
        let polished = arrange(Method::Polish, &nodes, &edges, &clean, 16.0 / 9.0, &uniform(&edges, LABEL), None);
        let far = nodes.iter().filter(|(id, _)| (polished[id].x - clean[id].x).abs() + (polished[id].y - clean[id].y).abs() > 700).count();
        assert_eq!(far, 0);
    }

    #[test]
    fn empty_and_single() {
        assert!(tidy(&[], &[], 16.0 / 9.0, &Labels::new()).is_empty());
        let id = NodeId::new();
        assert_eq!(tidy(&[(id, 0)], &[], 16.0 / 9.0, &Labels::new())[&id], Vec2::ZERO);
    }

    #[test]
    fn is_deterministic() {
        let (nodes, edges) = graph(14);
        assert_eq!(tidy(&nodes, &edges, 16.0 / 9.0, &uniform(&edges, LABEL)), tidy(&nodes, &edges, 16.0 / 9.0, &uniform(&edges, LABEL)));
    }

    #[test]
    fn follows_all_the_rules_on_a_generated_graph() {
        let (nodes, edges) = graph(24);
        let p = tidy(&nodes, &edges, 16.0 / 9.0, &uniform(&edges, LABEL));
        let c = centres(&p, &nodes);
        let ctx = ctx_of(&nodes, &edges, LABEL);
        // Rounding to whole pixels may not undo the search's result.
        assert_eq!(ctx.scan(&c).0, 0, "defects remain");
        let (w, h) = extent(&c);
        let aspect = w / h;
        assert!((0.9..3.0).contains(&aspect), "aspect {aspect}");
        let min_x = p.values().map(|v| v.x).min().unwrap();
        let min_y = p.values().map(|v| v.y).min().unwrap();
        assert_eq!((min_x, min_y), (0, 0));
    }

    #[test]
    fn a_single_type_still_works() {
        let (mut nodes, edges) = graph(12);
        nodes.iter_mut().for_each(|(_, k)| *k = 0);
        let p = tidy(&nodes, &edges, 16.0 / 9.0, &Labels::new());
        let ctx = ctx_of(&nodes, &edges, 0.0);
        assert_eq!(ctx.scan(&centres(&p, &nodes)).0, 0);
    }

    #[test]
    fn large_graphs_use_the_layered_drawing() {
        let (nodes, edges) = graph(MAX_NODES + 1);
        let ids: Vec<NodeId> = nodes.iter().map(|(id, _)| *id).collect();
        assert_eq!(tidy(&nodes, &edges, 16.0 / 9.0, &uniform(&edges, LABEL)), layered(&ids, &edges));
    }
}
