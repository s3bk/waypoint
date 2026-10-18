//! A first layout for a batch of new nodes (a layered drawing, after Sugiyama):
//! dependencies on the left, dependants to the right, one column per layer.
//!
//! 1. Layer: a node sits one column right of its right-most dependency.
//! 2. Order: long links get invisible stand-in nodes in the columns they cross, then
//!    columns are re-sorted by the average row of their neighbours, sweeping both
//!    ways, keeping the ordering with the fewest crossings.
//! 3. Rows: each node is pulled toward the average height of its neighbours, without
//!    overlapping the node above it.
//!
//! Deterministic: ties keep the order the nodes were given in.

use std::collections::BTreeMap;

use waypoint_model::{NODE_HEIGHT, NODE_WIDTH, NodeId, Place, Vec2};

use crate::room::{FLOW_GAP, MARGIN};

/// Distance between columns, left edge to left edge. More than the flow gap of a lone
/// node, so a link has room for its label.
pub const COLUMN_PITCH: i32 = NODE_WIDTH + FLOW_GAP + 40;
const SWEEPS: usize = 12;
const ROW_PASSES: usize = 8;

struct Item {
    /// The real node, or `None` for a stand-in on a long link.
    node: Option<NodeId>,
    layer: usize,
    /// Neighbours in the previous and the next column.
    left: Vec<usize>,
    right: Vec<usize>,
}

fn height(item: &Item) -> f64 {
    if item.node.is_some() { NODE_HEIGHT as f64 } else { 0.0 }
}

/// Top-left positions for `nodes`, with the top-left corner of the drawing at the
/// origin. `edges` are `(from, to)` pairs, `from` depends on `to`; pairs that are not
/// both in `nodes` are ignored.
pub fn layered(nodes: &[NodeId], edges: &[(NodeId, NodeId)]) -> BTreeMap<NodeId, Vec2> {
    let index: BTreeMap<NodeId, usize> = nodes.iter().enumerate().map(|(i, &n)| (n, i)).collect();
    let links: Vec<(usize, usize)> = edges
        .iter()
        .filter_map(|(f, t)| Some((*index.get(f)?, *index.get(t)?)))
        .filter(|(f, t)| f != t)
        .collect();

    // Layers: longest path from a node with no dependencies.
    let n = nodes.len();
    let mut layer = vec![0usize; n];
    for _ in 0..n {
        let mut changed = false;
        for &(from, to) in &links {
            if layer[from] < layer[to] + 1 {
                layer[from] = layer[to] + 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    // Items: the nodes, then stand-ins. A link from `from` (layer b) to `to` (layer a < b) becomes a chain.
    let mut items: Vec<Item> = (0..n).map(|i| Item { node: Some(nodes[i]), layer: layer[i], left: vec![], right: vec![] }).collect();
    for &(from, to) in &links {
        let mut prev = to;
        for l in layer[to] + 1..layer[from] {
            items.push(Item { node: None, layer: l, left: vec![], right: vec![] });
            let id = items.len() - 1;
            items[prev].right.push(id);
            items[id].left.push(prev);
            prev = id;
        }
        items[prev].right.push(from);
        items[from].left.push(prev);
    }
    let columns = items.iter().map(|i| i.layer).max().map_or(0, |m| m + 1);
    let mut order: Vec<Vec<usize>> = vec![vec![]; columns];
    for (i, item) in items.iter().enumerate() {
        order[item.layer].push(i);
    }

    // Order within columns.
    let mut best = order.clone();
    let mut best_crossings = crossings(&items, &order);
    for sweep in 0..SWEEPS {
        if sweep % 2 == 0 {
            for c in 1..columns {
                reorder(&items, &mut order, c, c - 1, true);
            }
        } else {
            for c in (0..columns.saturating_sub(1)).rev() {
                reorder(&items, &mut order, c, c + 1, false);
            }
        }
        let x = crossings(&items, &order);
        if x < best_crossings {
            best_crossings = x;
            best = order.clone();
        }
    }
    let order = best;

    // Rows (centres).
    let mut y = vec![0.0f64; items.len()];
    for col in &order {
        let mut at = 0.0;
        for &i in col {
            y[i] = at + height(&items[i]) / 2.0;
            at += height(&items[i]) + MARGIN as f64;
        }
    }
    for pass in 0..ROW_PASSES {
        let forward = pass % 2 == 0;
        let cols: Vec<usize> = if forward { (0..columns).collect() } else { (0..columns).rev().collect() };
        for c in cols {
            settle(&items, &order[c], &mut y);
        }
    }

    // Top-left corners, with the drawing's corner at the origin.
    let top = (0..n).map(|i| y[i] - NODE_HEIGHT as f64 / 2.0).fold(f64::INFINITY, f64::min);
    let mut out = BTreeMap::new();
    for i in 0..n {
        let pos = Vec2::new(layer[i] as i32 * COLUMN_PITCH, (y[i] - NODE_HEIGHT as f64 / 2.0 - top).round() as i32);
        out.insert(nodes[i], pos);
    }
    out
}

/// Sort column `c` by the average position, in `order`, of its neighbours in column `reference`.
/// Items without neighbours keep their place.
fn reorder(items: &[Item], order: &mut [Vec<usize>], c: usize, reference: usize, use_left: bool) {
    let rank: BTreeMap<usize, usize> = order[reference].iter().enumerate().map(|(r, &i)| (i, r)).collect();
    let mut keyed: Vec<(f64, usize)> = order[c]
        .iter()
        .enumerate()
        .map(|(own, &i)| {
            let nb = if use_left { &items[i].left } else { &items[i].right };
            let ranks: Vec<f64> = nb.iter().filter_map(|j| rank.get(j)).map(|&r| r as f64).collect();
            let key = if ranks.is_empty() { own as f64 } else { ranks.iter().sum::<f64>() / ranks.len() as f64 };
            (key, i)
        })
        .collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0)); // stable: ties keep their order
    order[c] = keyed.into_iter().map(|(_, i)| i).collect();
}

/// Crossing pairs of links between neighbouring columns.
fn crossings(items: &[Item], order: &[Vec<usize>]) -> usize {
    let mut total = 0;
    for c in 0..order.len().saturating_sub(1) {
        let rank: BTreeMap<usize, usize> = order[c + 1].iter().enumerate().map(|(r, &i)| (i, r)).collect();
        let mut segs: Vec<(usize, usize)> = Vec::new();
        for (r, &i) in order[c].iter().enumerate() {
            for j in &items[i].right {
                if let Some(&q) = rank.get(j) {
                    segs.push((r, q));
                }
            }
        }
        for a in 0..segs.len() {
            for b in a + 1..segs.len() {
                if (segs[a].0 < segs[b].0 && segs[a].1 > segs[b].1) || (segs[a].0 > segs[b].0 && segs[a].1 < segs[b].1) {
                    total += 1;
                }
            }
        }
    }
    total
}

/// Move the items of one column toward the average height of their neighbours, keeping their order and spacing.
fn settle(items: &[Item], col: &[usize], y: &mut [f64]) {
    if col.is_empty() {
        return;
    }
    let want: Vec<f64> = col
        .iter()
        .map(|&i| {
            let nb: Vec<f64> = items[i].left.iter().chain(&items[i].right).map(|&j| y[j]).collect();
            if nb.is_empty() { y[i] } else { nb.iter().sum::<f64>() / nb.len() as f64 }
        })
        .collect();
    let gap = |a: usize, b: usize| (height(&items[a]) + height(&items[b])) / 2.0 + MARGIN as f64;
    // Forward: no overlap with the item above.
    let mut pos = want.clone();
    for k in 1..col.len() {
        pos[k] = pos[k].max(pos[k - 1] + gap(col[k - 1], col[k]));
    }
    // Shift the whole column so the average miss is zero, then keep the spacing again.
    let shift = pos.iter().zip(&want).map(|(p, w)| p - w).sum::<f64>() / col.len() as f64;
    for p in pos.iter_mut() {
        *p -= shift;
    }
    for k in 1..col.len() {
        pos[k] = pos[k].max(pos[k - 1] + gap(col[k - 1], col[k]));
    }
    for (k, &i) in col.iter().enumerate() {
        y[i] = pos[k];
    }
}

/// Places for the laid-out `nodes`: each hangs off its nearest dependency, and the ones with no
/// dependency sit at `origin` plus their offset. `positions` are those of [`layered`].
pub fn as_places(positions: &BTreeMap<NodeId, Vec2>, edges: &[(NodeId, NodeId)], origin: Vec2) -> Vec<(NodeId, Place)> {
    positions
        .iter()
        .map(|(&n, &pos)| {
            let anchor = edges
                .iter()
                .filter(|(f, t)| *f == n && positions.contains_key(t))
                .map(|(_, t)| *t)
                .min_by_key(|t| (positions[t].y - pos.y).abs());
            let place = match anchor {
                Some(a) => Place { anchor: Some(a), off: pos - positions[&a] },
                None => Place::at_origin(origin + pos),
            };
            (n, place)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::derive_positions;

    fn ids(n: usize) -> Vec<NodeId> {
        (0..n).map(|_| NodeId::new()).collect()
    }

    fn overlap(a: Vec2, b: Vec2) -> bool {
        (a.x - b.x).abs() < NODE_WIDTH && (a.y - b.y).abs() < NODE_HEIGHT
    }

    #[test]
    fn dependants_sit_right_of_what_they_depend_on() {
        let n = ids(4);
        // 1 and 2 depend on 0, 3 depends on 1 and 0 (a long link).
        let edges = [(n[1], n[0]), (n[2], n[0]), (n[3], n[1]), (n[3], n[0])];
        let p = layered(&n, &edges);
        assert_eq!((p[&n[0]].x, p[&n[1]].x, p[&n[3]].x), (0, COLUMN_PITCH, 2 * COLUMN_PITCH));
        assert_eq!(p[&n[1]].x, p[&n[2]].x);
    }

    #[test]
    fn nothing_overlaps_and_the_origin_is_the_top_left() {
        let n = ids(30);
        let edges: Vec<_> = (1..30).map(|i| (n[i], n[(i * 7) % i])).collect();
        let p = layered(&n, &edges);
        let all: Vec<_> = p.values().copied().collect();
        for a in 0..all.len() {
            for b in a + 1..all.len() {
                assert!(!overlap(all[a], all[b]), "{:?} and {:?}", all[a], all[b]);
            }
        }
        assert_eq!((all.iter().map(|v| v.x).min(), all.iter().map(|v| v.y).min()), (Some(0), Some(0)));
    }

    #[test]
    fn a_chain_is_a_straight_row_and_a_fan_is_centred() {
        let n = ids(4);
        let chain = layered(&n, &[(n[1], n[0]), (n[2], n[1]), (n[3], n[2])]);
        assert!(chain.values().all(|v| v.y == 0));
        let m = ids(4);
        let fan = layered(&m, &[(m[1], m[0]), (m[2], m[0]), (m[3], m[0])]);
        let mid = (fan[&m[1]].y + fan[&m[3]].y) / 2;
        assert!((fan[&m[0]].y - mid).abs() <= 2, "the source sits between its dependants: {fan:?}");
    }

    #[test]
    fn a_crossing_is_untangled() {
        // Two chains whose given order would cross.
        let n = ids(4);
        let p = layered(&n, &[(n[2], n[1]), (n[3], n[0])]);
        let (a, b) = (p[&n[0]].y < p[&n[1]].y, p[&n[3]].y < p[&n[2]].y);
        assert_eq!(a, b, "no crossing: {p:?}");
    }

    #[test]
    fn places_derive_back_to_the_positions() {
        let n = ids(6);
        let edges = [(n[1], n[0]), (n[2], n[0]), (n[3], n[1]), (n[4], n[3]), (n[5], n[2])];
        let p = layered(&n, &edges);
        let places: BTreeMap<_, _> = as_places(&p, &edges, Vec2::new(100, 200)).into_iter().collect();
        let back = derive_positions(&places);
        for (id, pos) in &p {
            assert_eq!(back[id], *pos + Vec2::new(100, 200));
        }
    }

    #[test]
    fn empty_and_single() {
        assert!(layered(&[], &[]).is_empty());
        let n = ids(1);
        assert_eq!(layered(&n, &[])[&n[0]], Vec2::ZERO);
    }
}
