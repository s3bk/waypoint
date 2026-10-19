//! Keeping the anchor invariant (DATA_MODEL.md, "Layout", rule 2): a node's
//! anchor is a neighbour, that is, joined to it by a live edge. When edges go
//! away, the nodes left with an invalid anchor are re-anchored without moving.

use std::collections::{BTreeMap, BTreeSet};

use waypoint_model::{EdgeTypeId, NodeId, Place};

use crate::derive_positions;

fn neighbours(places: &BTreeMap<NodeId, Place>, edges: &[(NodeId, NodeId)]) -> BTreeMap<NodeId, BTreeSet<NodeId>> {
    let mut out: BTreeMap<NodeId, BTreeSet<NodeId>> = BTreeMap::new();
    for &(a, b) in edges {
        if a != b && places.contains_key(&a) && places.contains_key(&b) {
            out.entry(a).or_default().insert(b);
            out.entry(b).or_default().insert(a);
        }
    }
    out
}

/// Nodes anchored (directly or not) beneath `root`, including `root`.
fn anchor_subtree(places: &BTreeMap<NodeId, Place>, root: NodeId) -> BTreeSet<NodeId> {
    let mut children: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for (&n, p) in places {
        if let Some(a) = p.anchor {
            children.entry(a).or_default().push(n);
        }
    }
    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if seen.insert(n) {
            stack.extend(children.get(&n).into_iter().flatten().copied());
        }
    }
    seen
}

/// Re-anchor every node whose anchor is no longer one of its neighbours
/// (`edges` are the live edges, direction ignored).
///
/// Each such node moves to its nearest remaining neighbour that is not one of
/// its own sources, since that would make an anchor cycle, or to the scope
/// origin if there is none. Offsets are recomputed so no node changes
/// position. Returns only the places that change, sorted by id.
pub fn repair_anchors(places: &BTreeMap<NodeId, Place>, edges: &[(NodeId, NodeId)]) -> Vec<(NodeId, Place)> {
    let near = neighbours(places, edges);
    let pos = derive_positions(places);
    let mut work = places.clone();

    for (&n, original) in places {
        let Some(anchor) = original.anchor else { continue };
        if places.contains_key(&anchor) && near.get(&n).is_some_and(|s| s.contains(&anchor)) {
            continue;
        }
        let below = anchor_subtree(&work, n);
        let squared = |c: &NodeId| {
            let (dx, dy) = ((pos[&n].x - pos[c].x) as i64, (pos[&n].y - pos[c].y) as i64);
            dx * dx + dy * dy
        };
        let best = near
            .get(&n)
            .into_iter()
            .flatten()
            .filter(|c| !below.contains(c))
            .min_by_key(|c| (squared(c), **c))
            .copied();
        work.insert(
            n,
            match best {
                Some(c) => Place { anchor: Some(c), off: pos[&n] - pos[&c] },
                None => Place::at_origin(pos[&n]),
            },
        );
    }
    work.into_iter().filter(|(n, p)| places[n] != *p).collect()
}

/// Whether adding a link from `from` to `to` to `edges` (each `(x, y)` meaning x
/// links to y) would close a cycle. A self-link counts.
pub fn creates_cycle(edges: &[(NodeId, NodeId)], from: NodeId, to: NodeId) -> bool {
    if from == to {
        return true;
    }
    // A cycle forms iff `from` is already reachable from `to`.
    let mut next: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for &(a, b) in edges {
        next.entry(a).or_default().push(b);
    }
    let mut seen = BTreeSet::new();
    let mut stack = vec![to];
    while let Some(n) = stack.pop() {
        if n == from {
            return true;
        }
        if seen.insert(n) {
            stack.extend(next.get(&n).into_iter().flatten().copied());
        }
    }
    false
}

/// The edges to add when `node` is deleted so that chains through it survive:
/// for every `D -> node` and `node -> P` of the same edge type, `D -> P`.
/// Deleting B from A <- B <- C leaves C linking to A.
///
/// Edges are `(from, to, type)`. An edge is identified by its ordered pair, so a
/// pair that already has an edge (of any type) is skipped, as are self-links. Adding these cannot close a cycle: `D` already
/// reached `P` through `node`. Sorted, so the result is deterministic.
pub fn bridge_edges(edges: &[(NodeId, NodeId, EdgeTypeId)], node: NodeId) -> Vec<(NodeId, NodeId, EdgeTypeId)> {
    let existing: BTreeSet<(NodeId, NodeId)> = edges.iter().map(|e| (e.0, e.1)).collect();
    let into: Vec<_> = edges.iter().filter(|e| e.1 == node && e.0 != node).collect();
    let out: Vec<_> = edges.iter().filter(|e| e.0 == node && e.1 != node).collect();
    let mut bridges = BTreeSet::new();
    for &&(d, _, ty) in &into {
        for &&(_, p, out_ty) in &out {
            if ty == out_ty && d != p && !existing.contains(&(d, p)) {
                bridges.insert((d, p, ty));
            }
        }
    }
    bridges.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use waypoint_model::Vec2;

    fn v(x: i32, y: i32) -> Vec2 {
        Vec2::new(x, y)
    }

    fn apply(places: &BTreeMap<NodeId, Place>, changes: Vec<(NodeId, Place)>) -> BTreeMap<NodeId, Place> {
        let mut out = places.clone();
        out.extend(changes);
        out
    }

    /// root(0,0); a anchored to root at (200,0); b anchored to a at (400,0); c free at (400,300).
    fn scene() -> (Vec<NodeId>, BTreeMap<NodeId, Place>) {
        let n: Vec<NodeId> = (0..4).map(|_| NodeId::new()).collect();
        let mut p = BTreeMap::new();
        p.insert(n[0], Place::at_origin(v(0, 0)));
        p.insert(n[1], Place { anchor: Some(n[0]), off: v(200, 0) });
        p.insert(n[2], Place { anchor: Some(n[1]), off: v(200, 0) });
        p.insert(n[3], Place::at_origin(v(400, 300)));
        (n, p)
    }

    #[test]
    fn valid_anchors_are_left_alone() {
        let (n, p) = scene();
        let edges = [(n[1], n[0]), (n[2], n[1])];
        assert!(repair_anchors(&p, &edges).is_empty());
    }

    #[test]
    fn removing_the_anchor_edge_reanchors_to_the_nearest_neighbour_without_moving() {
        let (n, p) = scene();
        // b was anchored to a, but now only touches root (far) and c (near).
        let edges = [(n[1], n[0]), (n[2], n[0]), (n[2], n[3])];
        let before = derive_positions(&p);
        let changes = repair_anchors(&p, &edges);
        assert_eq!(changes.len(), 1);
        let out = apply(&p, changes);
        assert_eq!(out[&n[2]].anchor, Some(n[3]), "c at (400,300) is nearer to b (400,0) than root is");
        assert_eq!(derive_positions(&out), before, "nothing may move");
    }

    #[test]
    fn a_node_with_no_neighbours_goes_to_the_origin() {
        let (n, p) = scene();
        // a has lost every edge; b is linked only to root.
        let edges = [(n[2], n[0])];
        let before = derive_positions(&p);
        let out = apply(&p, repair_anchors(&p, &edges));
        assert_eq!(out[&n[1]], Place::at_origin(v(200, 0)));
        assert_eq!(derive_positions(&out), before);
    }

    #[test]
    fn never_anchors_a_node_to_its_own_source() {
        let (n, p) = scene();
        // a (anchored to root) loses root; its only neighbour is b, which hangs off a.
        let edges = [(n[2], n[1])];
        let out = apply(&p, repair_anchors(&p, &edges));
        assert_eq!(out[&n[1]], Place::at_origin(v(200, 0)));
        // The anchor graph still has no cycle: every node derives a position.
        assert_eq!(derive_positions(&out).len(), 4);
    }

    #[test]
    fn sources_follow_a_reanchored_node() {
        let (_, p) = scene();
        let before = derive_positions(&p);
        let out = apply(&p, repair_anchors(&p, &[]));
        assert_eq!(derive_positions(&out), before);
        assert!(out.values().all(|pl| pl.anchor.is_none()), "no edges means everything hangs off the origin");
    }

    #[test]
    fn cycle_detection() {
        let n: Vec<NodeId> = (0..4).map(|_| NodeId::new()).collect();
        let edges = [(n[0], n[1]), (n[1], n[2])]; // 0 -> 1 -> 2
        assert!(creates_cycle(&edges, n[0], n[0]), "self link");
        assert!(creates_cycle(&edges, n[2], n[0]), "2 links to 0 closes the loop");
        assert!(creates_cycle(&edges, n[1], n[0]), "direct reverse");
        assert!(!creates_cycle(&edges, n[0], n[2]), "a shortcut is fine");
        assert!(!creates_cycle(&edges, n[3], n[0]), "an unrelated node is fine");
        assert!(!creates_cycle(&edges, n[0], n[3]));
    }

    // ---- bridging edges when a node is deleted ----

    fn ty(n: u128) -> EdgeTypeId {
        EdgeTypeId(uuid_from(n))
    }

    fn uuid_from(n: u128) -> waypoint_model::Uuid {
        waypoint_model::Uuid::from_u128(n)
    }

    #[test]
    fn deleting_the_middle_of_a_chain_joins_its_ends() {
        let n: Vec<NodeId> = (0..3).map(|_| NodeId::new()).collect();
        // n0 -> n1 -> n2.
        let edges = [(n[0], n[1], ty(1)), (n[1], n[2], ty(1))];
        assert_eq!(bridge_edges(&edges, n[1]), vec![(n[0], n[2], ty(1))]);
    }

    #[test]
    fn several_sources_and_destinations_are_all_joined() {
        let n: Vec<NodeId> = (0..5).map(|_| NodeId::new()).collect();
        let edges = [
            (n[0], n[2], ty(1)),
            (n[1], n[2], ty(1)),
            (n[2], n[3], ty(1)),
            (n[2], n[4], ty(1)),
        ];
        assert_eq!(bridge_edges(&edges, n[2]).len(), 4, "2 sources x 2 destinations");
    }

    #[test]
    fn an_existing_shortcut_is_not_duplicated() {
        let n: Vec<NodeId> = (0..3).map(|_| NodeId::new()).collect();
        let edges = [(n[0], n[1], ty(1)), (n[1], n[2], ty(1)), (n[0], n[2], ty(1))];
        assert!(bridge_edges(&edges, n[1]).is_empty());
    }

    #[test]
    fn a_pair_that_already_has_an_edge_of_another_type_is_skipped() {
        let n: Vec<NodeId> = (0..3).map(|_| NodeId::new()).collect();
        // n0 already relates to n2 (type 2); joining through n1 must not add a second edge.
        let edges = [(n[0], n[1], ty(1)), (n[1], n[2], ty(1)), (n[0], n[2], ty(2))];
        assert!(bridge_edges(&edges, n[1]).is_empty());
    }

    #[test]
    fn only_edges_of_the_same_type_are_joined() {
        let n: Vec<NodeId> = (0..3).map(|_| NodeId::new()).collect();
        let edges = [(n[0], n[1], ty(1)), (n[1], n[2], ty(2))];
        assert!(bridge_edges(&edges, n[1]).is_empty());
    }

    #[test]
    fn nodes_at_the_end_of_a_chain_or_alone_bridge_nothing() {
        let n: Vec<NodeId> = (0..3).map(|_| NodeId::new()).collect();
        let edges = [(n[0], n[1], ty(1))];
        assert!(bridge_edges(&edges, n[0]).is_empty());
        assert!(bridge_edges(&edges, n[1]).is_empty());
        assert!(bridge_edges(&edges, n[2]).is_empty());
    }

    #[test]
    fn bridging_never_creates_a_cycle() {
        let n: Vec<NodeId> = (0..4).map(|_| NodeId::new()).collect();
        let mut edges = vec![(n[0], n[1], ty(1)), (n[1], n[2], ty(1)), (n[2], n[3], ty(1)), (n[0], n[3], ty(1))];
        let bridges = bridge_edges(&edges, n[1]);
        edges.extend(&bridges);
        edges.retain(|e| e.0 != n[1] && e.1 != n[1]);
        let pairs: Vec<_> = edges.iter().map(|e| (e.0, e.1)).collect();
        // Re-adding any existing destination must not look like a new cycle.
        for &(a, b) in &pairs {
            let others: Vec<_> = pairs.iter().copied().filter(|&e| e != (a, b)).collect();
            assert!(!creates_cycle(&others, a, b), "{a:?} -> {b:?} closes a cycle");
        }
    }
}
