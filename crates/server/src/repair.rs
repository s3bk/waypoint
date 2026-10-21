//! Post-merge repair (DATA_MODEL.md, "What a CRDT cannot do"). Clients write to
//! the document directly, so two of them can each make a change that is fine
//! alone and invalid once merged. After each merge the server looks for:
//!
//! - **self-links**: flagged `conflicted` (kept, visible, but out of `Snapshot::edges`). Cycles
//!   are fine: links have no direction as far as layout is concerned;
//! - **anchor cycles**: one member is moved to the scope origin;
//! - **invalid anchors**: a node anchored to something that is no longer its
//!   neighbour is re-anchored (the layout crate's `repair_anchors`).
//!
//! - **overlaps**: the newer of two overlapping nodes is pushed away;
//! - **dangling links**: a link to a node that was deleted (or never existed) is removed.
//!
//! The server is the only repair writer, and a
//! repaired document needs no further repair, so running it again is a no-op.

use std::collections::{BTreeMap, BTreeSet};

use waypoint_layout::{derive_positions, repair_anchors, repair_overlaps};
use waypoint_model::{NodeId, Place};

use crate::graph::{Snapshot, Write};

/// The writes that bring `snap` back to a valid state; empty if it already is.
pub fn plan(snap: &Snapshot) -> Vec<Write> {
    let mut writes = Vec::new();

    // Links whose end is gone: a link added to a node that someone else had just deleted.
    for &(from, to) in &snap.dangling {
        writes.push(Write::Unlink { from, to });
    }

    // Links may run in any direction and loop back; only a link to itself is set aside.
    for e in &snap.edges {
        if e.from == e.to {
            writes.push(Write::MarkConflicted { from: e.from, to: e.to });
        }
    }

    // Anchors. Edges that were set aside still join their ends on screen, so
    // they count as neighbours here.
    let mut places = snap.places();
    for (id, place) in break_anchor_cycles(&places) {
        places.insert(id, place);
        writes.push(Write::SetPlace { id, place });
    }
    let joined: Vec<_> = snap.edges.iter().chain(&snap.conflicted).map(|e| (e.from, e.to)).collect();
    for (id, place) in repair_anchors(&places, &joined) {
        places.insert(id, place);
        writes.push(Write::SetPlace { id, place });
    }
    // Overlaps, e.g. two clients adding at the same spot.
    for (id, place) in repair_overlaps(&places) {
        writes.push(Write::SetPlace { id, place });
    }
    writes
}

/// Places that break every anchor cycle. `derive_positions` shows the members of
/// a cycle as if each were anchored at the origin, so that is where they are;
/// the largest id becomes origin-anchored for real, and every member's offset is
/// recomputed so that nothing moves.
fn break_anchor_cycles(places: &BTreeMap<NodeId, Place>) -> Vec<(NodeId, Place)> {
    let anchor = |n: NodeId| places[&n].anchor.filter(|a| places.contains_key(a));
    let mut members = BTreeSet::new();
    let mut safe = BTreeSet::new();
    for &start in places.keys() {
        let mut walk = Vec::new();
        let mut cur = Some(start);
        while let Some(n) = cur {
            if safe.contains(&n) || members.contains(&n) {
                break;
            }
            if let Some(i) = walk.iter().position(|&w| w == n) {
                members.extend(walk[i..].iter().copied());
                walk.truncate(i);
                break;
            }
            walk.push(n);
            cur = anchor(n);
        }
        safe.extend(walk);
    }
    if members.is_empty() {
        return Vec::new();
    }
    // One root per cycle: its largest member. Finding cycles again from the
    // members keeps cycles separate.
    let mut roots = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for &m in &members {
        if seen.contains(&m) {
            continue;
        }
        let mut ring = vec![m];
        let mut cur = anchor(m).expect("cycle member is anchored");
        while cur != m {
            ring.push(cur);
            cur = anchor(cur).expect("cycle member is anchored");
        }
        seen.extend(ring.iter().copied());
        roots.insert(*ring.iter().max().expect("non-empty"));
    }
    let pos = derive_positions(places);
    members
        .into_iter()
        .map(|n| {
            let place = if roots.contains(&n) {
                Place::at_origin(pos[&n])
            } else {
                let a = anchor(n).expect("cycle member is anchored");
                Place { anchor: Some(a), off: pos[&n] - pos[&a] }
            };
            (n, place)
        })
        .filter(|(n, p)| places[n] != *p)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use waypoint_model::{EdgeTypeId, Vec2};

    fn node(g: &Graph, title: &str, place: Place) -> NodeId {
        let id = NodeId::new();
        g.create_node(id, title, place);
        id
    }

    fn repair(g: &Graph) -> Vec<Write> {
        let writes = plan(&g.snapshot());
        g.apply_writes(&writes);
        writes
    }

    fn sync(from: &Graph, to: &Graph) {
        to.apply_update(&from.encode_state()).unwrap();
    }

    #[test]
    fn nodes_added_to_the_same_spot_are_pushed_apart_once() {
        let (alice, bob) = (Graph::new(), Graph::new());
        let a = node(&alice, "a", Place::at_origin(Vec2::ZERO));
        let b = node(&bob, "b", Place::at_origin(Vec2::ZERO));
        let server = Graph::new();
        sync(&alice, &server);
        sync(&bob, &server);
        assert_eq!(repair(&server).len(), 1);
        let pos = derive_positions(&server.places());
        assert_eq!(pos[&a.min(b)], Vec2::ZERO, "the older node stays");
        assert_ne!(pos[&a.max(b)], Vec2::ZERO);
        assert!(repair(&server).is_empty());
    }

    #[test]
    fn a_valid_graph_needs_no_repair() {
        let g = Graph::new();
        g.seed_demo();
        assert!(plan(&g.snapshot()).is_empty());
    }

    #[test]
    fn links_that_close_a_cycle_are_kept() {
        let base = Graph::new();
        let a = node(&base, "a", Place::at_origin(Vec2::ZERO));
        let b = node(&base, "b", Place { anchor: Some(a), off: Vec2::new(200, 0) });
        let (alice, bob) = (Graph::new(), Graph::new());
        sync(&base, &alice);
        sync(&base, &bob);
        alice.create_edge(a, b, EdgeTypeId::DEFAULT); // a -> b
        bob.create_edge(b, a, EdgeTypeId::DEFAULT); // b -> a
        let server = Graph::new();
        for g in [&base, &alice, &bob] {
            sync(g, &server);
        }
        assert!(repair(&server).is_empty(), "a loop is a valid graph");
        let snap = server.snapshot();
        assert_eq!((snap.edges.len(), snap.conflicted.len()), (2, 0));
    }

    #[test]
    fn an_old_flag_on_a_cycle_link_is_ignored() {
        let g = Graph::new();
        let a = node(&g, "a", Place::at_origin(Vec2::ZERO));
        let b = node(&g, "b", Place::at_origin(Vec2::new(400, 0)));
        g.create_edge(a, b, EdgeTypeId::DEFAULT);
        g.create_edge(b, a, EdgeTypeId::DEFAULT);
        g.apply_writes(&[Write::MarkConflicted { from: b, to: a }]);
        let snap = g.snapshot();
        assert_eq!((snap.edges.len(), snap.conflicted.len()), (2, 0));
    }

    #[test]
    fn a_self_link_is_set_aside() {
        let g = Graph::new();
        let a = node(&g, "a", Place::at_origin(Vec2::ZERO));
        g.create_edge(a, a, EdgeTypeId::DEFAULT);
        repair(&g);
        assert_eq!((g.snapshot().edges.len(), g.snapshot().conflicted.len()), (0, 1));
        assert!(repair(&g).is_empty(), "repair is idempotent");
    }

    #[test]
    fn concurrent_re_anchoring_into_a_loop_is_broken() {
        let base = Graph::new();
        let a = node(&base, "a", Place::at_origin(Vec2::new(100, 100)));
        let b = node(&base, "b", Place { anchor: Some(a), off: Vec2::new(200, 0) });
        let c = node(&base, "c", Place { anchor: Some(b), off: Vec2::new(200, 0) });
        base.create_edge(b, a, EdgeTypeId::DEFAULT);
        base.create_edge(c, b, EdgeTypeId::DEFAULT);
        base.create_edge(a, c, EdgeTypeId::DEFAULT); // closes a cycle too
        let (alice, bob) = (Graph::new(), Graph::new());
        sync(&base, &alice);
        sync(&base, &bob);
        // Alice anchors a to c at the spot it already has; Bob does nothing else.
        let pos = derive_positions(&alice.places());
        alice.apply_writes(&[Write::SetPlace { id: a, place: Place { anchor: Some(c), off: pos[&a] - pos[&c] } }]);
        let server = Graph::new();
        sync(&alice, &server);
        sync(&bob, &server);

        repair(&server);
        assert!(repair(&server).is_empty());
        // Nobody is left anchored in a loop: everything reaches the origin.
        let places = server.places();
        for &n in places.keys() {
            let mut cur = n;
            for _ in 0..places.len() {
                match places[&cur].anchor {
                    Some(next) => cur = next,
                    None => break,
                }
            }
            assert!(places[&cur].anchor.is_none(), "{n:?} still in a loop");
        }
    }

    #[test]
    fn an_anchor_that_is_no_longer_a_neighbour_is_re_anchored_in_place() {
        let base = Graph::new();
        let a = node(&base, "a", Place::at_origin(Vec2::new(0, 0)));
        let b = node(&base, "b", Place { anchor: Some(a), off: Vec2::new(200, 0) });
        let c = node(&base, "c", Place { anchor: Some(b), off: Vec2::new(200, 0) });
        base.create_edge(b, a, EdgeTypeId::DEFAULT);
        base.create_edge(c, b, EdgeTypeId::DEFAULT);
        let (alice, bob) = (Graph::new(), Graph::new());
        sync(&base, &alice);
        sync(&base, &bob);
        // Alice removes the link that anchors c; Bob adds a link c to a.
        alice.apply_writes(&[Write::Unlink { from: c, to: b }]);
        bob.create_edge(c, a, EdgeTypeId::DEFAULT);
        let server = Graph::new();
        for g in [&base, &alice, &bob] {
            sync(g, &server);
        }
        let before = derive_positions(&server.places());
        repair(&server);
        assert_eq!(server.places()[&c].anchor, Some(a), "c's only neighbour is now a");
        assert_eq!(derive_positions(&server.places()), before);
        assert!(repair(&server).is_empty());
    }

    #[test]
    fn deleting_the_anchor_on_one_side_while_another_node_is_added_below_it() {
        let base = Graph::new();
        let a = node(&base, "a", Place::at_origin(Vec2::ZERO));
        let (alice, bob) = (Graph::new(), Graph::new());
        sync(&base, &alice);
        sync(&base, &bob);
        alice.apply_writes(&[Write::DeleteNode { id: a }]);
        let late = node(&bob, "late", Place { anchor: Some(a), off: Vec2::new(200, 50) });
        let server = Graph::new();
        for g in [&base, &alice, &bob] {
            sync(g, &server);
        }
        repair(&server);
        assert_eq!(server.places()[&late], Place::at_origin(Vec2::new(200, 50)), "valid, and not moved");
        assert!(repair(&server).is_empty());
    }

    #[test]
    fn a_link_to_a_deleted_node_is_removed() {
        let (alice, bob) = (Graph::new(), Graph::new());
        let a = node(&alice, "a", Place::at_origin(Vec2::new(0, 0)));
        let b = node(&alice, "b", Place { anchor: Some(a), off: Vec2::new(240, 0) });
        bob.apply_update(&alice.encode_state()).unwrap();
        // Alice deletes b while bob links a to it.
        alice.apply_writes(&[Write::DeleteNode { id: b }]);
        bob.create_edge(a, b, EdgeTypeId::DEFAULT);
        alice.apply_update(&bob.encode_state()).unwrap();
        assert_eq!(alice.snapshot().dangling, vec![(a, b)]);
        alice.apply_writes(&plan(&alice.snapshot()));
        assert!(alice.snapshot().dangling.is_empty(), "the link is gone");
        assert!(plan(&alice.snapshot()).is_empty(), "and repair is done");
    }
}
