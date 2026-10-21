//! Layout for Waypoint: derives absolute positions from stored places.
//! Placement, make-room, repair and tidy will live here too (see
//! DATA_MODEL.md, "Layout"). Everything is pure and deterministic so the
//! server (native) and the client (WASM) agree.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use waypoint_model::{NodeId, Place, Vec2};

mod anchors;
mod initial;
mod room;
mod tidy;
pub use anchors::{bridge_edges, creates_cycle, repair_anchors};
pub use initial::{COLUMN_PITCH, as_places, layered};
pub use tidy::{LABEL_CHAR_WIDTH, Labels, MAX_NODES, Method, arrange, defect_count, tidy};
pub use room::{EDGE_CLEARANCE, FLOW_GAP, MARGIN, MAX_WRITES, add_node, drop_node, make_room, repair_overlaps};

#[cfg(target_arch = "wasm32")]
mod wasm;

/// Derive each node's absolute position within its scope.
///
/// `pos(node) = pos(anchor) + off`, and `pos(None) = (0, 0)`. An anchor that
/// is not in `places` is treated as the origin. Members of an anchor cycle
/// are treated as anchored at the origin until the server repairs the cycle,
/// so the result is always total and deterministic. Iterative, so deep
/// anchor chains cannot overflow the stack.
pub fn derive_positions(places: &BTreeMap<NodeId, Place>) -> BTreeMap<NodeId, Vec2> {
    let mut pos: HashMap<NodeId, Vec2> = HashMap::with_capacity(places.len());
    let mut path: Vec<NodeId> = Vec::new();

    for &start in places.keys() {
        if pos.contains_key(&start) {
            continue;
        }
        path.clear();
        let mut cur = start;
        // Walk anchors until we reach a known position, the origin, or a loop.
        loop {
            if pos.contains_key(&cur) {
                break;
            }
            if let Some(i) = path.iter().position(|&n| n == cur) {
                // Cycle: its members become origin-anchored.
                for &member in &path[i..] {
                    pos.insert(member, places[&member].off);
                }
                path.truncate(i);
                break;
            }
            path.push(cur);
            match places[&cur].anchor.filter(|a| places.contains_key(a)) {
                Some(next) => cur = next,
                None => {
                    // Origin-anchored (or anchor missing): resolve this one directly.
                    let n = path.pop().expect("just pushed");
                    pos.insert(n, places[&n].off);
                    break;
                }
            }
        }
        // Unwind: each path node's anchor is now known.
        while let Some(n) = path.pop() {
            let place = places[&n];
            let anchor_pos = place.anchor.and_then(|a| pos.get(&a)).copied().unwrap_or(Vec2::ZERO);
            pos.insert(n, anchor_pos + place.off);
        }
    }
    pos.into_iter().collect()
}

/// The places that change when `id` is dragged so that it ends up at
/// `target` (absolute, in the scope).
///
/// - `with_subtree`: the node and everything anchored beneath it move
///   together. One place changes: the node's own offset.
/// - otherwise only the node moves. Nodes anchored directly to it have their
///   offsets compensated so they stay where they are; deeper nodes follow
///   their (unmoved) anchors. Cost is 1 + the number of direct sources.
///
/// Returns only places that differ from `places`, sorted by id. Empty if the
/// node is unknown or already at `target`.
pub fn move_node(
    places: &BTreeMap<NodeId, Place>,
    id: NodeId,
    target: Vec2,
    with_subtree: bool,
) -> Vec<(NodeId, Place)> {
    let before = derive_positions(places);
    let Some(&current) = before.get(&id) else { return Vec::new() };
    let delta = target - current;
    if delta == Vec2::ZERO {
        return Vec::new();
    }

    let mut after = places.clone();
    let moved = after.get_mut(&id).expect("id is in places");
    moved.off = moved.off + delta;

    if !with_subtree {
        // Measure rather than assume `-delta`: a direct source that sits in
        // an anchor cycle is origin-anchored and did not move at all.
        let now = derive_positions(&after);
        let sources: Vec<NodeId> =
            places.iter().filter(|(_, p)| p.anchor == Some(id)).map(|(&n, _)| n).collect();
        for n in sources {
            let shift = before[&n] - now[&n];
            let child = after.get_mut(&n).expect("source is in places");
            child.off = child.off + shift;
        }
    }

    after.into_iter().filter(|(n, p)| places[n] != *p).collect()
}

/// `id` and everything it links to, directly or not. An edge `(from, to)` means "from" links to "to".
pub fn destination_group(edges: &[(NodeId, NodeId)], id: NodeId) -> BTreeSet<NodeId> {
    let mut group = BTreeSet::from([id]);
    let mut queue = vec![id];
    while let Some(n) = queue.pop() {
        for &(from, to) in edges {
            if from == n && group.insert(to) {
                queue.push(to);
            }
        }
    }
    group
}

/// What a drag with Shift takes along with `id`: the parts of the graph that hang off it.
///
/// Take `id` out and the links (in either direction) split the rest into pieces. The largest
/// piece is the main body and stays; every other piece moves with `id`. When the largest piece
/// is less than [`BODY_RATIO`] times as big as all the others together there is no clear main
/// body and everything connected moves. A node that is not a cut point (one piece or none) takes
/// what it links to, directly or not ([`destination_group`]).
pub fn drag_group(edges: &[(NodeId, NodeId)], id: NodeId) -> BTreeSet<NodeId> {
    let mut near: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for &(from, to) in edges {
        if from != to && from != id && to != id {
            near.entry(from).or_default().push(to);
            near.entry(to).or_default().push(from);
        }
    }
    let mut seen = BTreeSet::from([id]);
    let mut pieces: Vec<BTreeSet<NodeId>> = Vec::new();
    let starts: BTreeSet<NodeId> =
        edges.iter().filter_map(|&(f, t)| if f == id { Some(t) } else if t == id { Some(f) } else { None }).filter(|&n| n != id).collect();
    for start in starts {
        if !seen.insert(start) {
            continue;
        }
        let (mut piece, mut stack) = (BTreeSet::from([start]), vec![start]);
        while let Some(n) = stack.pop() {
            for &m in near.get(&n).into_iter().flatten() {
                if seen.insert(m) {
                    piece.insert(m);
                    stack.push(m);
                }
            }
        }
        pieces.push(piece);
    }
    if pieces.len() < 2 {
        return destination_group(edges, id);
    }
    // Largest first; equal sizes keep the order of their smallest ids, so the result is stable.
    pieces.sort_by(|a, b| b.len().cmp(&a.len()));
    let rest: usize = pieces[1..].iter().map(BTreeSet::len).sum();
    let skip = if pieces[0].len() as f64 >= BODY_RATIO * rest as f64 { 1 } else { 0 };
    let mut group = BTreeSet::from([id]);
    for piece in &pieces[skip..] {
        group.extend(piece);
    }
    group
}

/// How many times larger than all the other pieces the largest must be to count as the main body.
pub const BODY_RATIO: f64 = 1.5;

/// The places that change when `id` is dragged to `target` together with its group
/// ([`drag_group`]), which moves rigidly by the same offset.
///
/// A member whose anchor is outside the group takes the offset in its own place;
/// a member anchored to another member follows it. A node outside the group that
/// is anchored to a member would follow too, so its offset is compensated and it
/// stays where it was. Returns only places that differ, sorted by id.
pub fn move_group(
    places: &BTreeMap<NodeId, Place>,
    edges: &[(NodeId, NodeId)],
    id: NodeId,
    target: Vec2,
) -> Vec<(NodeId, Place)> {
    let before = derive_positions(places);
    let Some(&current) = before.get(&id) else { return Vec::new() };
    let delta = target - current;
    if delta == Vec2::ZERO {
        return Vec::new();
    }
    let group = drag_group(edges, id);
    let mut after = places.clone();
    for (n, place) in after.iter_mut() {
        let member = group.contains(n);
        let follows = place.anchor.is_some_and(|a| group.contains(&a));
        if member && !follows {
            place.off = place.off + delta;
        } else if !member && follows {
            place.off = place.off - delta;
        }
    }
    after.into_iter().filter(|(n, p)| places[n] != *p).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<NodeId> {
        (0..n).map(|_| NodeId::new()).collect()
    }

    fn v(x: i32, y: i32) -> Vec2 {
        Vec2::new(x, y)
    }

    #[test]
    fn origin_and_chain() {
        let n = ids(3);
        let mut places = BTreeMap::new();
        places.insert(n[0], Place::at_origin(v(10, 20)));
        places.insert(n[1], Place { anchor: Some(n[0]), off: v(5, 0) });
        places.insert(n[2], Place { anchor: Some(n[1]), off: v(0, -7) });
        let pos = derive_positions(&places);
        assert_eq!(pos[&n[0]], v(10, 20));
        assert_eq!(pos[&n[1]], v(15, 20));
        assert_eq!(pos[&n[2]], v(15, 13));
    }

    #[test]
    fn missing_anchor_is_origin() {
        let n = ids(2);
        let mut places = BTreeMap::new();
        places.insert(n[0], Place { anchor: Some(n[1]), off: v(3, 4) });
        assert_eq!(derive_positions(&places)[&n[0]], v(3, 4));
    }

    #[test]
    fn cycle_members_anchor_at_origin() {
        let n = ids(3);
        let mut places = BTreeMap::new();
        places.insert(n[0], Place { anchor: Some(n[1]), off: v(1, 0) });
        places.insert(n[1], Place { anchor: Some(n[0]), off: v(2, 0) });
        // n[2] hangs off the cycle.
        places.insert(n[2], Place { anchor: Some(n[0]), off: v(0, 9) });
        let pos = derive_positions(&places);
        assert_eq!(pos[&n[0]], v(1, 0));
        assert_eq!(pos[&n[1]], v(2, 0));
        assert_eq!(pos[&n[2]], v(1, 9));
    }

    #[test]
    fn deep_chain_does_not_overflow() {
        let n = ids(200_000);
        let mut places = BTreeMap::new();
        places.insert(n[0], Place::at_origin(v(0, 0)));
        for i in 1..n.len() {
            places.insert(n[i], Place { anchor: Some(n[i - 1]), off: v(1, 0) });
        }
        let pos = derive_positions(&places);
        assert_eq!(pos[&n[n.len() - 1]], v(n.len() as i32 - 1, 0));
    }

    /// a(10,20) <- b(+100,0) <- c(+100,0) <- d(+0,50), plus an unrelated e.
    fn chain() -> (Vec<NodeId>, BTreeMap<NodeId, Place>) {
        let n = ids(5);
        let mut places = BTreeMap::new();
        places.insert(n[0], Place::at_origin(v(10, 20)));
        places.insert(n[1], Place { anchor: Some(n[0]), off: v(100, 0) });
        places.insert(n[2], Place { anchor: Some(n[1]), off: v(100, 0) });
        places.insert(n[3], Place { anchor: Some(n[2]), off: v(0, 50) });
        places.insert(n[4], Place::at_origin(v(-300, -300)));
        (n, places)
    }

    fn apply(places: &BTreeMap<NodeId, Place>, changes: Vec<(NodeId, Place)>) -> BTreeMap<NodeId, Place> {
        let mut out = places.clone();
        out.extend(changes);
        out
    }

    #[test]
    fn drag_with_subtree_is_one_write_and_moves_sources() {
        let (n, places) = chain();
        let changes = move_node(&places, n[1], v(150, 40), true);
        assert_eq!(changes.len(), 1);
        let pos = derive_positions(&apply(&places, changes));
        assert_eq!(pos[&n[0]], v(10, 20)); // anchor untouched
        assert_eq!(pos[&n[1]], v(150, 40)); // moved by (+40,+40)
        assert_eq!(pos[&n[2]], v(250, 40)); // followed
        assert_eq!(pos[&n[3]], v(250, 90)); // followed
        assert_eq!(pos[&n[4]], v(-300, -300));
    }

    #[test]
    fn drag_single_node_leaves_sources_in_place() {
        let (n, places) = chain();
        let before = derive_positions(&places);
        let changes = move_node(&places, n[1], v(150, 40), false);
        assert_eq!(changes.len(), 2); // the node, and its one direct source
        let pos = derive_positions(&apply(&places, changes));
        assert_eq!(pos[&n[1]], v(150, 40));
        for i in [0, 2, 3, 4] {
            assert_eq!(pos[&n[i]], before[&n[i]], "node {i} must not move");
        }
    }

    #[test]
    fn drag_root_single_with_several_sources() {
        let (n, mut places) = chain();
        places.insert(n[4], Place { anchor: Some(n[0]), off: v(0, 80) });
        let before = derive_positions(&places);
        let changes = move_node(&places, n[0], v(0, 0), false);
        assert_eq!(changes.len(), 3); // root + two direct sources
        let pos = derive_positions(&apply(&places, changes));
        assert_eq!(pos[&n[0]], v(0, 0));
        for i in 1..5 {
            assert_eq!(pos[&n[i]], before[&n[i]]);
        }
    }

    #[test]
    fn drag_to_same_place_or_unknown_node_changes_nothing() {
        let (n, places) = chain();
        assert!(move_node(&places, n[1], v(110, 20), true).is_empty());
        assert!(move_node(&places, NodeId::new(), v(0, 0), true).is_empty());
    }

    #[test]
    fn drag_single_node_in_a_cycle_does_not_move_others() {
        let n = ids(2);
        let mut places = BTreeMap::new();
        places.insert(n[0], Place { anchor: Some(n[1]), off: v(1, 0) });
        places.insert(n[1], Place { anchor: Some(n[0]), off: v(50, 0) });
        let before = derive_positions(&places);
        let pos = derive_positions(&apply(&places, move_node(&places, n[0], v(9, 9), false)));
        assert_eq!(pos[&n[0]], v(9, 9));
        assert_eq!(pos[&n[1]], before[&n[1]]);
    }

    #[test]
    fn deterministic_regardless_of_insertion_order() {
        let n = ids(4);
        let mut a = BTreeMap::new();
        let mut b = BTreeMap::new();
        let entries = [
            (n[0], Place { anchor: Some(n[1]), off: v(1, 1) }),
            (n[1], Place { anchor: Some(n[0]), off: v(1, 1) }),
            (n[2], Place { anchor: Some(n[3]), off: v(2, 2) }),
            (n[3], Place::at_origin(v(7, 7))),
        ];
        for e in entries { a.insert(e.0, e.1); }
        for e in entries.iter().rev() { b.insert(e.0, e.1); }
        assert_eq!(derive_positions(&a), derive_positions(&b));
    }

    /// A path of `len` new nodes joined to `to` (links point along the path, away from `to`).
    fn arm(edges: &mut Vec<(NodeId, NodeId)>, to: NodeId, len: usize) -> Vec<NodeId> {
        let nodes = ids(len);
        let mut prev = to;
        for &n in &nodes {
            edges.push((n, prev));
            prev = n;
        }
        nodes
    }

    #[test]
    fn a_drag_leaves_the_main_body_and_takes_the_small_pieces() {
        let n = ids(1);
        let hub = n[0];
        let mut edges = Vec::new();
        let body = arm(&mut edges, hub, 10);
        // A smaller piece that loops back to the hub, with a tail behind it.
        let side = ids(3);
        edges.extend([(hub, side[0]), (side[1], side[0]), (side[1], side[2]), (side[2], hub)]);
        let tail = arm(&mut edges, side[1], 2);
        let group = drag_group(&edges, hub);
        let want: BTreeSet<_> = [hub].into_iter().chain(side).chain(tail).collect();
        assert_eq!(group, want);
        assert!(body.iter().all(|b| !group.contains(b)));
    }

    #[test]
    fn a_leaf_behind_the_node_is_taken_whichever_way_its_link_points() {
        let hub = NodeId::new();
        let mut edges = Vec::new();
        arm(&mut edges, hub, 8);
        let (leaf_in, leaf_out) = (NodeId::new(), NodeId::new());
        edges.push((leaf_in, hub));
        edges.push((hub, leaf_out));
        assert_eq!(drag_group(&edges, hub), BTreeSet::from([hub, leaf_in, leaf_out]));
    }

    #[test]
    fn without_a_clear_main_body_everything_connected_moves() {
        let hub = NodeId::new();
        let mut edges = Vec::new();
        let (a, b) = (arm(&mut edges, hub, 5), arm(&mut edges, hub, 4));
        let group = drag_group(&edges, hub);
        assert_eq!(group.len(), 10);
        assert!(a.iter().chain(&b).all(|n| group.contains(n)));
    }

    #[test]
    fn a_node_that_is_not_a_cut_point_takes_what_it_links_to() {
        // a -> b, a -> c, b -> d, c -> d: no node separates the rest.
        let n = ids(4);
        let (a, b, c, d) = (n[0], n[1], n[2], n[3]);
        let edges = [(a, b), (a, c), (b, d), (c, d)];
        assert_eq!(drag_group(&edges, a), BTreeSet::from([a, b, c, d]));
        assert_eq!(drag_group(&edges, d), BTreeSet::from([d]));
    }

    /// The shape of a real 63-node graph (links only: nodes are numbered 1..=63, no text). Two clusters
    /// hang off node 2 and node 5 on opposite sides; a third island is nodes 37..=46.
    const REAL_SHAPE: [(u32, u32); 89] = [
    (5, 2), (6, 3), (7, 5), (8, 1), (9, 5), (9, 6), (9, 8), (10, 9), (12, 11), (13, 8),
    (13, 11), (14, 13), (15, 14), (16, 5), (16, 6), (16, 7), (16, 9), (16, 14), (17, 9), (17, 10),
    (17, 11), (17, 12), (17, 16), (18, 11), (18, 16), (19, 15), (19, 16), (19, 17), (19, 18), (20, 4),
    (20, 19), (21, 20), (22, 20), (22, 21), (23, 20), (24, 20), (25, 22), (25, 23), (25, 24), (26, 25),
    (27, 26), (28, 9), (28, 27), (29, 22), (29, 23), (29, 24), (29, 28), (30, 26), (30, 28), (30, 29),
    (31, 29), (32, 22), (33, 29), (33, 32), (34, 17), (35, 23), (36, 11), (37, 40), (37, 41), (38, 39),
    (39, 40), (40, 43), (40, 44), (41, 42), (42, 38), (43, 44), (45, 43), (45, 44), (46, 44), (48, 47),
    (49, 48), (50, 49), (51, 50), (52, 2), (52, 51), (54, 52), (54, 53), (55, 54), (56, 2), (56, 55),
    (59, 56), (59, 57), (59, 58), (60, 59), (61, 59), (62, 55), (62, 60), (62, 61), (63, 62),
    ];

    fn real_shape() -> (Vec<NodeId>, Vec<(NodeId, NodeId)>) {
        let ids = ids(64); // index 0 unused
        (ids.clone(), REAL_SHAPE.iter().map(|&(f, t)| (ids[f as usize], ids[t as usize])).collect())
    }

    #[test]
    #[ignore]
    fn bounds_on_real_shape() {
        let (ids, edges) = real_shape();
        let nodes: Vec<(NodeId, u32)> = ids[1..].iter().enumerate().map(|(i, &id)| (id, (i / 12) as u32)).collect();
        // Only the two links that carry a label (the real graph has two) need room for text.
        let labels: Labels = edges.iter().filter(|(f, _)| [ids[41], ids[53]].contains(f)).map(|&e| (e, 8.0 * LABEL_CHAR_WIDTH)).collect();
        for (bw, bh) in [(1728.0, 972.0), (2000.0, 1125.0), (2400.0, 1350.0)] {
            let t = std::time::Instant::now();
            let p = arrange(Method::Force, &nodes, &edges, &BTreeMap::new(), 16.0 / 9.0, &labels, Some((bw, bh)));
            let xs = p.values().map(|v| v.x).max().unwrap() + 160;
            let ys = p.values().map(|v| v.y).max().unwrap() + 56;
            println!("{bw}x{bh}: drawing {xs} x {ys} in {:?}, {} defects", t.elapsed(), defect_count(&nodes, &edges, &p, &labels));
        }
    }

    #[test]
    fn dragging_on_a_real_graph_takes_the_part_that_hangs_off_the_node() {
        let (ids, edges) = real_shape();
        let group = |n: usize| -> BTreeSet<u32> {
            let g = drag_group(&edges, ids[n]);
            (1..64u32).filter(|&k| g.contains(&ids[k as usize])).collect()
        };
        // Node 2: the installer side (47..=63) comes along, the pipeline behind node 5 stays.
        assert_eq!(group(2), (47..=63).chain([2]).collect());
        // Node 51: the chain it depends on.
        assert_eq!(group(51), BTreeSet::from([47, 48, 49, 50, 51]));
        // Node 54: its leaf (53).
        assert_eq!(group(54), BTreeSet::from([53, 54]));
        // Node 40: the whole island, since neither side is clearly the main body.
        assert_eq!(group(40), (37..=46).collect());
        // Node 44: the leaf behind it (46).
        assert_eq!(group(44), BTreeSet::from([44, 46]));
    }

    #[test]
    fn group_drag_moves_what_the_node_links_to_and_nothing_else() {
        // main links to dep (dep is not anchored to main); other hangs off main by its anchor only.
        let n = ids(4);
        let (main, dep, deeper, other) = (n[0], n[1], n[2], n[3]);
        let mut places = BTreeMap::new();
        places.insert(main, Place::at_origin(v(0, 0)));
        places.insert(dep, Place::at_origin(v(300, 0)));
        places.insert(deeper, Place { anchor: Some(dep), off: v(0, 100) });
        places.insert(other, Place { anchor: Some(main), off: v(0, 200) });
        let edges = [(main, dep), (dep, deeper)];
        let moved = apply(&places, move_group(&places, &edges, main, v(10, 50)));
        let pos = derive_positions(&moved);
        assert_eq!(pos[&main], v(10, 50));
        assert_eq!(pos[&dep], v(310, 50), "a destination moves along");
        assert_eq!(pos[&deeper], v(310, 150), "and what it links to");
        assert_eq!(pos[&other], v(0, 200), "an unrelated node anchored under main stays put");
        assert_eq!(destination_group(&edges, dep), BTreeSet::from([dep, deeper]), "main is not a destination of dep");
    }
}
