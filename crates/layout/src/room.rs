//! Making room: when a node is added or dropped, nodes in its way are pushed
//! aside (DATA_MODEL.md, "Layout", rule 5).
//!
//! Pushing moves a node and the nodes anchored beneath it as one rigid body,
//! so a push usually costs one write however many nodes ride along. Pushes
//! ripple: a moved cluster can now overlap something else, which is pushed in
//! turn. Everything here is deterministic (ties broken by node id).
//!
//! Cost: each push re-derives all positions, so a pass is O(budget * n). That
//! is fine for hundreds of nodes; a spatial index is the next step for
//! thousands (DESIGN.md, open questions).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use waypoint_model::{NODE_HEIGHT, NODE_WIDTH, NodeId, Place, Vec2};

use crate::{derive_positions, move_node};

/// Free space kept between neighbouring nodes.
pub const MARGIN: i32 = 24;
/// Gap between a node and a dependant added next to it.
pub const FLOW_GAP: i32 = 80;
/// A pass stops once this many places have been written; leftover overlap is
/// left for an explicit tidy.
pub const MAX_WRITES: usize = 50;

fn overlaps(a: Vec2, b: Vec2) -> bool {
    (a.x - b.x).abs() < NODE_WIDTH + MARGIN && (a.y - b.y).abs() < NODE_HEIGHT + MARGIN
}

/// The shortest axis-aligned move that takes `victim` out of `source`'s way,
/// away from it. Keeps the order of the two along the other axis, so nodes
/// never swap sides.
fn push_vector(source: Vec2, victim: Vec2) -> Vec2 {
    let (dx, dy) = (victim.x - source.x, victim.y - source.y);
    let (px, py) = (NODE_WIDTH + MARGIN - dx.abs(), NODE_HEIGHT + MARGIN - dy.abs());
    if px <= py {
        Vec2::new(if dx < 0 { -px } else { px }, 0)
    } else {
        Vec2::new(0, if dy < 0 { -py } else { py })
    }
}

/// `anchor -> nodes anchored to it`.
fn children_of(places: &BTreeMap<NodeId, Place>) -> BTreeMap<NodeId, Vec<NodeId>> {
    let mut children: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for (&n, p) in places {
        if let Some(a) = p.anchor {
            children.entry(a).or_default().push(n);
        }
    }
    children
}

/// Whether `root`'s anchor subtree contains any node in `set`.
fn subtree_touches(root: NodeId, children: &BTreeMap<NodeId, Vec<NodeId>>, set: &BTreeSet<NodeId>) -> bool {
    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if !seen.insert(n) {
            continue;
        }
        if set.contains(&n) {
            return true;
        }
        stack.extend(children.get(&n).into_iter().flatten().copied());
    }
    false
}

/// Places in `after` that differ from `before` (or are new), sorted by id.
fn diff(before: &BTreeMap<NodeId, Place>, after: &BTreeMap<NodeId, Place>) -> Vec<(NodeId, Place)> {
    after.iter().filter(|(n, p)| before.get(n) != Some(p)).map(|(&n, &p)| (n, p)).collect()
}

/// Free space kept between an edge's line and a node beside it.
pub const EDGE_CLEARANCE: i32 = 16;
/// Nudging a node off edges can reveal a second edge behind the first; this
/// bounds how often we retry.
const MAX_EDGE_PASSES: usize = 6;

type Pt = (f64, f64);

fn centre(p: Vec2) -> Pt {
    (p.x as f64 + NODE_WIDTH as f64 / 2.0, p.y as f64 + NODE_HEIGHT as f64 / 2.0)
}

/// Where the ray from `from`'s centre toward `to` leaves `from`'s box. Matches
/// how the client draws an edge, which stops at the node borders.
fn box_exit(from: Pt, to: Pt) -> Pt {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    if dx == 0.0 && dy == 0.0 {
        return from;
    }
    let tx = if dx == 0.0 { f64::INFINITY } else { NODE_WIDTH as f64 / 2.0 / dx.abs() };
    let ty = if dy == 0.0 { f64::INFINITY } else { NODE_HEIGHT as f64 / 2.0 / dy.abs() };
    let t = tx.min(ty);
    (from.0 + dx * t, from.1 + dy * t)
}

/// Does segment `p`-`q` touch the node box at `at`, grown by the edge clearance?
fn segment_hits_node(p: Pt, q: Pt, at: Vec2) -> bool {
    let c = EDGE_CLEARANCE as f64;
    let (x0, x1) = (at.x as f64 - c, (at.x + NODE_WIDTH) as f64 + c);
    let (y0, y1) = (at.y as f64 - c, (at.y + NODE_HEIGHT) as f64 + c);
    // Liang-Barsky clipping of the segment against the rectangle.
    let (dx, dy) = (q.0 - p.0, q.1 - p.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (pk, qk) in [(-dx, p.0 - x0), (dx, x1 - p.0), (-dy, p.1 - y0), (dy, y1 - p.1)] {
        if pk == 0.0 {
            if qk < 0.0 {
                return false;
            }
        } else {
            let r = qk / pk;
            if pk < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

/// How far, and which way, to move the node at `at` so that segment `p`-`q`
/// no longer touches it: straight across the edge, to the side the node's
/// centre is already on, by just enough.
fn clear_of_segment(p: Pt, q: Pt, at: Vec2) -> Option<Vec2> {
    let (ux, uy) = (q.0 - p.0, q.1 - p.1);
    let len = ux.hypot(uy);
    if len == 0.0 {
        return None;
    }
    let (nx, ny) = (-uy / len, ux / len);
    let m = centre(at);
    let side = (m.0 - p.0) * nx + (m.1 - p.1) * ny;
    let c = EDGE_CLEARANCE as f64;
    let reach = (NODE_WIDTH as f64 / 2.0 + c) * nx.abs() + (NODE_HEIGHT as f64 / 2.0 + c) * ny.abs();
    let need = reach - side.abs() + 1.0;
    let sign = if side < 0.0 { -1.0 } else { 1.0 };
    // Round away from zero so the move can only overshoot.
    let away = |v: f64| if v < 0.0 { v.floor() as i32 } else { v.ceil() as i32 };
    Some(Vec2::new(away(nx * sign * need), away(ny * sign * need)))
}

/// If any node in `moved` sits on an edge that does not touch the moved
/// group, the move that clears the first such edge.
fn edge_nudge(
    pos: &BTreeMap<NodeId, Vec2>,
    edges: &[(NodeId, NodeId)],
    moved: &BTreeSet<NodeId>,
) -> Option<Vec2> {
    let mut edges: Vec<_> = edges.iter().filter(|(a, b)| !moved.contains(a) && !moved.contains(b)).collect();
    edges.sort();
    for &&(a, b) in &edges {
        let (Some(&pa), Some(&pb)) = (pos.get(&a), pos.get(&b)) else { continue };
        let (ca, cb) = (centre(pa), centre(pb));
        let (p, q) = (box_exit(ca, cb), box_exit(cb, ca));
        for &m in moved {
            if let Some(&at) = pos.get(&m) {
                if segment_hits_node(p, q, at) {
                    if let Some(d) = clear_of_segment(p, q, at) {
                        return Some(d);
                    }
                }
            }
        }
    }
    None
}

/// Move `id` (with its sub-graph, or alone) off any edge it lies on. `moved`
/// is the group that moves rigidly with it. Returns the updated places.
fn nudge_off_edges(
    mut cur: BTreeMap<NodeId, Place>,
    edges: &[(NodeId, NodeId)],
    id: NodeId,
    moved: &BTreeSet<NodeId>,
    with_subtree: bool,
) -> BTreeMap<NodeId, Place> {
    for _ in 0..MAX_EDGE_PASSES {
        let pos = derive_positions(&cur);
        let Some(d) = edge_nudge(&pos, edges, moved) else { break };
        let changes = move_node(&cur, id, pos[&id] + d, with_subtree);
        if changes.is_empty() {
            break;
        }
        cur.extend(changes);
    }
    cur
}

/// Push apart everything that overlaps the `fixed` nodes (the ones just
/// placed or dropped). Fixed nodes never move. Returns the places that change.
pub fn make_room(places: &BTreeMap<NodeId, Place>, fixed: &BTreeSet<NodeId>) -> Vec<(NodeId, Place)> {
    let children = children_of(places);
    let mut cur = places.clone();
    let mut pos = derive_positions(&cur);
    let mut pushed: BTreeSet<NodeId> = BTreeSet::new();
    let mut touched: BTreeSet<NodeId> = BTreeSet::new();
    let mut queue: VecDeque<NodeId> = fixed.iter().copied().collect();

    while let Some(src) = queue.pop_front() {
        let Some(&src_pos) = pos.get(&src) else { continue };
        let victims: Vec<NodeId> = pos
            .iter()
            .filter(|(n, p)| **n != src && !fixed.contains(n) && !pushed.contains(n) && overlaps(src_pos, **p))
            .map(|(&n, _)| n)
            .collect();

        for n in victims {
            // An earlier push in this loop may already have cleared it.
            if !overlaps(src_pos, pos[&n]) {
                continue;
            }
            if touched.len() >= MAX_WRITES {
                return diff(places, &cur);
            }
            let d = push_vector(src_pos, pos[&n]);
            let before = pos.clone();

            // Move the node; its whole anchor subtree follows... except a branch
            // that holds a fixed node, which must stay: compensate that branch.
            let moved = cur.get_mut(&n).expect("victim is in places");
            moved.off = moved.off + d;
            touched.insert(n);
            for &c in children.get(&n).into_iter().flatten() {
                if subtree_touches(c, &children, fixed) {
                    let child = cur.get_mut(&c).expect("child is in places");
                    child.off = child.off - d;
                    touched.insert(c);
                }
            }

            pos = derive_positions(&cur);
            pushed.insert(n);
            // Whatever moved may now be in someone else's way.
            queue.extend(pos.iter().filter(|(m, p)| before[*m] != **p).map(|(&m, _)| m));
        }
    }
    diff(places, &cur)
}

/// Resolve overlaps that nobody was placed to resolve, such as two clients
/// adding nodes to the same spot at once. Nodes are taken oldest first (ids are
/// time-ordered): an older node stays put and the newer ones it overlaps are
/// pushed away, so every replica computes the same answer. Returns the places
/// that change; empty if nothing overlaps, and a second run changes nothing
/// unless the write budget ran out.
pub fn repair_overlaps(places: &BTreeMap<NodeId, Place>) -> Vec<(NodeId, Place)> {
    let ids: Vec<NodeId> = places.keys().copied().collect();
    let mut cur = places.clone();
    let mut fixed = BTreeSet::new();
    for (i, &n) in ids.iter().enumerate() {
        fixed.insert(n);
        let pos = derive_positions(&cur);
        if !ids[i + 1..].iter().any(|m| overlaps(pos[&n], pos[m])) {
            continue;
        }
        let pushes = make_room(&cur, &fixed);
        cur.extend(pushes);
    }
    diff(places, &cur)
}

/// Add node `id` and make room for it. With a `parent`, the node goes next to
/// it in the flow direction (to its right, as a dependant); otherwise at `at`.
/// A node that would land on an unrelated edge is moved off it. `edges` are the
/// live edges as `(from, to)`. Returns the new node's place and the places of
/// everything pushed.
pub fn add_node(
    places: &BTreeMap<NodeId, Place>,
    edges: &[(NodeId, NodeId)],
    id: NodeId,
    parent: Option<NodeId>,
    at: Vec2,
) -> Vec<(NodeId, Place)> {
    let place = match parent.filter(|p| places.contains_key(p)) {
        Some(p) => Place { anchor: Some(p), off: Vec2::new(NODE_WIDTH + FLOW_GAP, 0) },
        None => Place::at_origin(at),
    };
    let mut cur = places.clone();
    cur.insert(id, place);
    let group = BTreeSet::from([id]);
    let mut cur = nudge_off_edges(cur, edges, id, &group, true);
    let pushes = make_room(&cur, &group);
    cur.extend(pushes);
    diff(places, &cur)
}

/// Drop node `id` at `target` (see [`move_node`]), move it off any unrelated
/// edge it landed on, and make room for whatever moved. A drop that changes nothing leaves the layout alone, even if nodes
/// already overlap.
pub fn drop_node(
    places: &BTreeMap<NodeId, Place>,
    edges: &[(NodeId, NodeId)],
    id: NodeId,
    target: Vec2,
    with_subtree: bool,
) -> Vec<(NodeId, Place)> {
    let moved = move_node(places, id, target, with_subtree);
    if moved.is_empty() {
        return moved;
    }
    let mut cur = places.clone();
    cur.extend(moved);
    let (before, after) = (derive_positions(places), derive_positions(&cur));
    let fixed: BTreeSet<NodeId> = after.iter().filter(|(n, p)| before.get(n) != Some(p)).map(|(&n, _)| n).collect();
    // The dropped node yields to edges, then everything else yields to it.
    let mut cur = nudge_off_edges(cur, edges, id, &fixed, with_subtree);
    let pushes = make_room(&cur, &fixed);
    cur.extend(pushes);
    diff(places, &cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: i32, y: i32) -> Vec2 {
        Vec2::new(x, y)
    }

    fn at(places: &mut BTreeMap<NodeId, Place>, x: i32, y: i32) -> NodeId {
        let id = NodeId::new();
        places.insert(id, Place::at_origin(v(x, y)));
        id
    }

    fn apply(places: &BTreeMap<NodeId, Place>, changes: Vec<(NodeId, Place)>) -> BTreeMap<NodeId, Place> {
        let mut out = places.clone();
        out.extend(changes);
        out
    }

    fn assert_no_overlaps(places: &BTreeMap<NodeId, Place>) {
        let pos = derive_positions(places);
        let all: Vec<_> = pos.iter().collect();
        for (i, (a, pa)) in all.iter().enumerate() {
            for (b, pb) in &all[i + 1..] {
                assert!(!overlaps(**pa, **pb), "{a:?} at {pa:?} overlaps {b:?} at {pb:?}");
            }
        }
    }

    const STEP: i32 = NODE_WIDTH + MARGIN; // closest spacing that does not overlap

    #[test]
    fn nothing_to_do_when_nothing_overlaps() {
        let mut p = BTreeMap::new();
        let a = at(&mut p, 0, 0);
        at(&mut p, STEP, 0);
        assert!(make_room(&p, &BTreeSet::from([a])).is_empty());
    }

    #[test]
    fn pushes_the_overlapped_node_away_along_the_shortest_axis() {
        let mut p = BTreeMap::new();
        let fixed = at(&mut p, 0, 0);
        let other = at(&mut p, 50, 10); // overlaps; shorter way out is down
        let out = apply(&p, make_room(&p, &BTreeSet::from([fixed])));
        let pos = derive_positions(&out);
        assert_eq!(pos[&fixed], v(0, 0), "the fixed node must not move");
        assert_eq!(pos[&other], v(50, NODE_HEIGHT + MARGIN)); // pushed down just clear of it
        assert_no_overlaps(&out);
    }

    #[test]
    fn pushing_keeps_sides() {
        let mut p = BTreeMap::new();
        let fixed = at(&mut p, 0, 0);
        let left = at(&mut p, -40, 5);
        let out = apply(&p, make_room(&p, &BTreeSet::from([fixed])));
        let pos = derive_positions(&out);
        assert!(pos[&left].x < pos[&fixed].x, "a node pushed away must stay on its side");
    }

    #[test]
    fn pushes_ripple_along_a_tight_row() {
        let mut p = BTreeMap::new();
        let row: Vec<_> = (1..=4).map(|i| at(&mut p, i * STEP, 0)).collect();
        // 120 to the left of the first node: the shortest way out is sideways.
        let fixed = at(&mut p, STEP - 120, 0);
        let changes = make_room(&p, &BTreeSet::from([fixed]));
        assert_eq!(changes.len(), 4, "every node in the row must be pushed in turn");
        let out = apply(&p, changes);
        assert_no_overlaps(&out);
        let pos = derive_positions(&out);
        // Order along the row is unchanged.
        let xs: Vec<i32> = row.iter().map(|n| pos[n].x).collect();
        assert!(xs.windows(2).all(|w| w[0] < w[1]), "{xs:?}");
    }

    #[test]
    fn an_anchored_cluster_moves_as_one_write() {
        let mut p = BTreeMap::new();
        let fixed = at(&mut p, 0, 0);
        let head = at(&mut p, 60, 0);
        // Three nodes hang off `head`, to its right.
        let tail: Vec<_> = (1..=3)
            .map(|i| {
                let id = NodeId::new();
                p.insert(id, Place { anchor: Some(head), off: v(i * STEP, 0) });
                id
            })
            .collect();
        let before = derive_positions(&p);
        let changes = make_room(&p, &BTreeSet::from([fixed]));
        assert_eq!(changes.len(), 1, "only the cluster's root is written");
        let out = apply(&p, changes);
        let pos = derive_positions(&out);
        let shift = pos[&head] - before[&head];
        for t in tail {
            assert_eq!(pos[&t] - before[&t], shift, "followers keep their shape");
        }
        assert_no_overlaps(&out);
    }

    #[test]
    fn a_branch_holding_a_fixed_node_stays_put() {
        let mut p = BTreeMap::new();
        let root = at(&mut p, 0, 0);
        // The fixed node hangs off `root` but overlaps it.
        let fixed = NodeId::new();
        p.insert(fixed, Place { anchor: Some(root), off: v(50, 10) });
        let before = derive_positions(&p);
        let out = apply(&p, make_room(&p, &BTreeSet::from([fixed])));
        let pos = derive_positions(&out);
        assert_eq!(pos[&fixed], before[&fixed], "the fixed node must not move");
        assert_ne!(pos[&root], before[&root], "its anchor is pushed out of the way");
        assert_no_overlaps(&out);
    }

    #[test]
    fn writes_are_capped_and_leftover_overlap_is_tolerated() {
        let mut p = BTreeMap::new();
        for i in 1..=200 {
            at(&mut p, i * STEP, 0);
        }
        let fixed = at(&mut p, STEP - 120, 0);
        let changes = make_room(&p, &BTreeSet::from([fixed]));
        assert_eq!(changes.len(), MAX_WRITES, "the ripple runs until the budget is spent");
        assert_eq!(changes.len(), changes.iter().map(|c| c.0).collect::<BTreeSet<_>>().len());
    }

    #[test]
    fn adding_a_dependant_places_it_to_the_right_and_pushes_the_occupant() {
        let mut p = BTreeMap::new();
        let parent = at(&mut p, 0, 0);
        let occupant = at(&mut p, NODE_WIDTH + FLOW_GAP, 0); // exactly where the new node goes
        let new = NodeId::new();
        let out = apply(&p, add_node(&p, &[], new, Some(parent), v(0, 0)));
        let pos = derive_positions(&out);
        assert_eq!(pos[&new], v(NODE_WIDTH + FLOW_GAP, 0));
        assert_ne!(pos[&occupant], v(NODE_WIDTH + FLOW_GAP, 0));
        assert_no_overlaps(&out);
    }

    #[test]
    fn adding_a_free_node_lands_where_asked() {
        let mut p = BTreeMap::new();
        at(&mut p, 500, 500);
        let new = NodeId::new();
        let changes = add_node(&p, &[], new, None, v(10, 20));
        assert_eq!(changes, vec![(new, Place::at_origin(v(10, 20)))]);
    }

    #[test]
    fn dropping_onto_a_node_pushes_it_and_the_dropped_node_wins() {
        let mut p = BTreeMap::new();
        let dragged = at(&mut p, 0, 0);
        let target = at(&mut p, 400, 0);
        let out = apply(&p, drop_node(&p, &[], dragged, v(400, 5), false));
        let pos = derive_positions(&out);
        assert_eq!(pos[&dragged], v(400, 5));
        assert_ne!(pos[&target], v(400, 0));
        assert_no_overlaps(&out);
    }

    #[test]
    fn a_drop_that_moves_nothing_leaves_overlaps_alone() {
        let mut p = BTreeMap::new();
        let a = at(&mut p, 0, 0);
        at(&mut p, 10, 10); // already overlapping
        assert!(drop_node(&p, &[], a, v(0, 0), false).is_empty());
    }

    #[test]
    fn dropping_a_whole_sub_graph_pushes_what_it_lands_on() {
        let mut p = BTreeMap::new();
        let head = at(&mut p, 0, 0);
        let follower = NodeId::new();
        p.insert(follower, Place { anchor: Some(head), off: v(STEP, 0) });
        let bystander = at(&mut p, 1000, STEP);
        // Drag the pair so the follower lands on the bystander.
        let out = apply(&p, drop_node(&p, &[], head, v(1000 - STEP, STEP), true));
        let pos = derive_positions(&out);
        assert_eq!(pos[&follower], v(1000, STEP), "the sub-graph keeps its shape");
        assert_ne!(pos[&bystander], v(1000, STEP));
        assert_no_overlaps(&out);
    }

    // ---- edges as obstacles ----

    /// Does the node at `at` touch the (drawn) edge between nodes `a` and `b`?
    fn touches_edge(places: &BTreeMap<NodeId, Place>, a: NodeId, b: NodeId, n: NodeId) -> bool {
        let pos = derive_positions(places);
        let (ca, cb) = (centre(pos[&a]), centre(pos[&b]));
        segment_hits_node(box_exit(ca, cb), box_exit(cb, ca), pos[&n])
    }

    /// Two far-apart nodes joined by a horizontal edge, and a free node below.
    fn edge_scene() -> (BTreeMap<NodeId, Place>, NodeId, NodeId, NodeId) {
        let mut p = BTreeMap::new();
        let a = at(&mut p, 0, 0);
        let b = at(&mut p, 1000, 0);
        let d = at(&mut p, 500, 600);
        (p, a, b, d)
    }

    #[test]
    fn dropping_a_node_on_an_edge_moves_it_off_the_edge() {
        let (p, a, b, d) = edge_scene();
        let edges = [(a, b)];
        let out = apply(&p, drop_node(&p, &edges, d, v(500, 0), false));
        assert!(!touches_edge(&out, a, b, d), "still on the edge");
        let pos = derive_positions(&out);
        assert_eq!(pos[&d].x, 500, "a horizontal edge pushes straight up or down");
        // Smallest move that clears the line: half the node plus the clearance.
        let moved = pos[&d].y.abs();
        assert!(moved >= NODE_HEIGHT / 2 + EDGE_CLEARANCE && moved <= NODE_HEIGHT / 2 + EDGE_CLEARANCE + 3, "moved {moved}");
        assert_eq!(pos[&a], v(0, 0));
        assert_eq!(pos[&b], v(1000, 0));
    }

    #[test]
    fn the_node_leaves_on_the_side_it_was_dropped_towards() {
        let (p, a, b, d) = edge_scene();
        let edges = [(a, b)];
        let above = derive_positions(&apply(&p, drop_node(&p, &edges, d, v(500, -10), false)))[&d];
        let below = derive_positions(&apply(&p, drop_node(&p, &edges, d, v(500, 10), false)))[&d];
        assert!(above.y < 0 && below.y > 0, "above {above:?}, below {below:?}");
    }

    #[test]
    fn diagonal_edges_are_cleared_too() {
        let mut p = BTreeMap::new();
        let a = at(&mut p, 0, 0);
        let b = at(&mut p, 1000, 700);
        let d = at(&mut p, 0, 900);
        let out = apply(&p, drop_node(&p, &[(a, b)], d, v(450, 320), false));
        assert!(!touches_edge(&out, a, b, d));
    }

    #[test]
    fn dropping_clear_of_edges_lands_exactly_where_asked() {
        let (p, a, b, d) = edge_scene();
        let out = apply(&p, drop_node(&p, &[(a, b)], d, v(500, 300), false));
        assert_eq!(derive_positions(&out)[&d], v(500, 300));
    }

    #[test]
    fn a_nodes_own_edges_do_not_push_it() {
        let (p, a, _b, d) = edge_scene();
        // d is linked to a; dropping d right next to a on its own edge is fine.
        let out = apply(&p, drop_node(&p, &[(d, a)], d, v(300, 10), false));
        assert_eq!(derive_positions(&out)[&d], v(300, 10));
    }

    #[test]
    fn a_dropped_sub_graph_moves_off_an_edge_as_one() {
        let mut p = BTreeMap::new();
        let a = at(&mut p, 0, 0);
        let b = at(&mut p, 2000, 0);
        let head = at(&mut p, 0, 800);
        let tail = NodeId::new();
        p.insert(tail, Place { anchor: Some(head), off: v(STEP, 0) });
        let edges = [(a, b)];
        let out = apply(&p, drop_node(&p, &edges, head, v(900, 0), true));
        let pos = derive_positions(&out);
        assert!(!touches_edge(&out, a, b, head) && !touches_edge(&out, a, b, tail));
        assert_eq!(pos[&tail] - pos[&head], v(STEP, 0), "the pair keeps its shape");
    }

    #[test]
    fn a_new_node_placed_on_an_edge_is_moved_off_it() {
        let mut p = BTreeMap::new();
        let parent = at(&mut p, 0, 0);
        // A vertical edge right where the new dependant would go (x = 240).
        let top = at(&mut p, 240, -500);
        let bottom = at(&mut p, 240, 500);
        let new = NodeId::new();
        let out = apply(&p, add_node(&p, &[(top, bottom)], new, Some(parent), v(0, 0)));
        assert!(!touches_edge(&out, top, bottom, new));
    }

    #[test]
    fn overlaps_are_repaired_oldest_first_and_once() {
        let mut places = BTreeMap::new();
        let a = at(&mut places, 0, 0);
        let b = at(&mut places, 0, 0);
        let c = at(&mut places, 10, 10);
        let moved = repair_overlaps(&places);
        assert!(moved.iter().all(|(id, _)| *id != a.min(b).min(c)), "the oldest stays");
        let fixed = apply(&places, moved);
        assert_no_overlaps(&fixed);
        assert!(repair_overlaps(&fixed).is_empty());
    }
}
