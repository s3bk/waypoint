//! Layout for Waypoint: derives absolute positions from stored places.
//! Placement, make-room, repair and tidy will live here too (see
//! DATA_MODEL.md, "Layout"). Everything is pure and deterministic so the
//! server (native) and the client (WASM) agree.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use waypoint_model::{NodeId, Place, Vec2};

mod anchors;
mod initial;
mod room;
pub use anchors::{bridge_edges, creates_cycle, repair_anchors};
pub use initial::{COLUMN_PITCH, as_places, layered};
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
///   their (unmoved) anchors. Cost is 1 + the number of direct dependants.
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
        // Measure rather than assume `-delta`: a direct dependant that sits in
        // an anchor cycle is origin-anchored and did not move at all.
        let now = derive_positions(&after);
        let dependants: Vec<NodeId> =
            places.iter().filter(|(_, p)| p.anchor == Some(id)).map(|(&n, _)| n).collect();
        for n in dependants {
            let shift = before[&n] - now[&n];
            let child = after.get_mut(&n).expect("dependant is in places");
            child.off = child.off + shift;
        }
    }

    after.into_iter().filter(|(n, p)| places[n] != *p).collect()
}

/// `id` and everything it depends on, directly or not. An edge `(from, to)` means "from depends on to".
pub fn dependency_group(edges: &[(NodeId, NodeId)], id: NodeId) -> BTreeSet<NodeId> {
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

/// The places that change when `id` is dragged to `target` together with all
/// it depends on ([`dependency_group`]), which move rigidly by the same offset.
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
    let group = dependency_group(edges, id);
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
    fn drag_with_subtree_is_one_write_and_moves_dependants() {
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
    fn drag_single_node_leaves_dependants_in_place() {
        let (n, places) = chain();
        let before = derive_positions(&places);
        let changes = move_node(&places, n[1], v(150, 40), false);
        assert_eq!(changes.len(), 2); // the node, and its one direct dependant
        let pos = derive_positions(&apply(&places, changes));
        assert_eq!(pos[&n[1]], v(150, 40));
        for i in [0, 2, 3, 4] {
            assert_eq!(pos[&n[i]], before[&n[i]], "node {i} must not move");
        }
    }

    #[test]
    fn drag_root_single_with_several_dependants() {
        let (n, mut places) = chain();
        places.insert(n[4], Place { anchor: Some(n[0]), off: v(0, 80) });
        let before = derive_positions(&places);
        let changes = move_node(&places, n[0], v(0, 0), false);
        assert_eq!(changes.len(), 3); // root + two direct dependants
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

    #[test]
    fn group_drag_moves_what_the_node_depends_on_and_nothing_else() {
        // main depends on dep (dep is not anchored to main); other hangs off main by its anchor only.
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
        assert_eq!(pos[&dep], v(310, 50), "a dependency moves along");
        assert_eq!(pos[&deeper], v(310, 150), "and what it depends on");
        assert_eq!(pos[&other], v(0, 200), "an unrelated node anchored under main stays put");
        assert_eq!(dependency_group(&edges, dep), BTreeSet::from([dep, deeper]), "main is not a dependency of dep");
    }
}
