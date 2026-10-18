//! Agent operations: plan a batch against a snapshot of the graph.
//!
//! Planning reads a [`Snapshot`], applies each op to an in-memory working copy
//! (using the layout crate for placement, making room, re-anchoring and joining
//! links), and records the resulting [`Write`]s. Nothing touches the document
//! until the whole batch has planned successfully, so a batch is atomic: if any
//! op fails, none of it is applied.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use uuid::Uuid;
use waypoint_layout as layout;
use waypoint_model::{EdgeTypeId, NODE_HEIGHT, NODE_WIDTH, NodeId, Op, Place, Vec2};

use crate::graph::{EdgeRec, NodeRec, Snapshot, Write};

pub const MAX_OPS: usize = 100;
const MAX_TITLE: usize = 200;
const MAX_NOTE: usize = 20_000;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct OpError {
    /// Index of the failing op in the batch.
    pub op_index: usize,
    pub message: String,
}

pub struct Plan {
    pub writes: Vec<Write>,
    /// One entry per op. `create_node` results get their `key` added once the
    /// server has numbered the new node.
    pub results: Vec<Value>,
}

struct Work {
    nodes: BTreeMap<NodeId, NodeRec>,
    places: BTreeMap<NodeId, Place>,
    edges: Vec<EdgeRec>,
    /// Set aside by the server for closing a cycle; they can only be removed.
    conflicted: Vec<EdgeRec>,
    writes: Vec<Write>,
    /// The node each op created, by op index, for `$N` references.
    created: Vec<Option<NodeId>>,
}

/// A node's note may hold this many characters.
fn check_note(text: &str) -> Result<(), String> {
    if text.chars().count() > MAX_NOTE {
        Err(format!("a note may hold at most {MAX_NOTE} characters"))
    } else {
        Ok(())
    }
}

pub fn plan(snapshot: &Snapshot, ops: &[Op]) -> Result<Plan, OpError> {
    if ops.len() > MAX_OPS {
        return Err(OpError { op_index: 0, message: format!("a batch may hold at most {MAX_OPS} ops") });
    }
    let mut work = Work {
        nodes: snapshot.nodes.clone(),
        places: snapshot.places(),
        edges: snapshot.edges.clone(),
        conflicted: snapshot.conflicted.clone(),
        writes: Vec::new(),
        created: Vec::with_capacity(ops.len()),
    };
    let mut results = Vec::with_capacity(ops.len());
    for (op_index, op) in ops.iter().enumerate() {
        let result = work.apply(op).map_err(|message| OpError { op_index, message })?;
        results.push(result);
    }
    Ok(Plan { writes: work.writes, results })
}

/// A readable name for error messages: `#3 "design"`.
fn describe(n: &NodeRec) -> String {
    match n.key {
        Some(k) => format!("#{k} \"{}\"", n.title),
        None => format!("\"{}\"", n.title),
    }
}

fn normalize_title(title: &str) -> Result<String, String> {
    let t: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = t.chars().take(MAX_TITLE).collect();
    if t.is_empty() { Err("title must not be empty".into()) } else { Ok(t) }
}

/// Turn a node reference into an id: a key (`"#12"` or `"12"`), a UUID, or
/// `"$N"` for the node created by op N of the current batch.
pub fn resolve_ref(
    nodes: &BTreeMap<NodeId, NodeRec>,
    created: &[Option<NodeId>],
    reference: &str,
) -> Result<NodeId, String> {
    let r = reference.trim();
    if let Some(index) = r.strip_prefix('$') {
        let i: usize = index.parse().map_err(|_| format!("bad reference {r:?}"))?;
        return created
            .get(i)
            .copied()
            .flatten()
            .ok_or_else(|| format!("{r} does not refer to a create_node earlier in this batch"));
    }
    let digits = r.strip_prefix('#').unwrap_or(r);
    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
        let key: u64 = digits.parse().map_err(|_| format!("bad node key {r:?}"))?;
        return nodes
            .values()
            .find(|n| n.key == Some(key))
            .map(|n| n.id)
            .ok_or_else(|| format!("no node with key #{key} (it may have been deleted)"));
    }
    let id = r
        .parse::<Uuid>()
        .map(NodeId)
        .map_err(|_| format!("{r:?} is not a node reference: use a key like \"#12\", an id, or \"$0\""))?;
    if nodes.contains_key(&id) || created.contains(&Some(id)) {
        Ok(id)
    } else {
        Err(format!("no live node with id {}", id.0))
    }
}

impl Work {
    fn resolve(&self, reference: &str) -> Result<NodeId, String> {
        resolve_ref(&self.nodes, &self.created, reference)
    }

    fn resolve_all(&self, refs: &[String]) -> Result<Vec<NodeId>, String> {
        let mut out: Vec<NodeId> = Vec::new();
        for r in refs {
            let id = self.resolve(r)?;
            if !out.contains(&id) {
                out.push(id);
            }
        }
        Ok(out)
    }

    fn name(&self, id: NodeId) -> String {
        self.nodes.get(&id).map(describe).unwrap_or_else(|| format!("{id:?}"))
    }

    fn pairs(&self) -> Vec<(NodeId, NodeId)> {
        self.edges.iter().map(|e| (e.from, e.to)).collect()
    }

    /// Record new places for nodes (the output of a layout function).
    fn set_places(&mut self, changes: Vec<(NodeId, Place)>, skip_create: Option<NodeId>) {
        for (id, place) in changes {
            self.places.insert(id, place);
            if let Some(n) = self.nodes.get_mut(&id) {
                n.place = place;
            }
            if Some(id) != skip_create {
                self.writes.push(Write::SetPlace { id, place });
            }
        }
    }

    /// Add "from depends on to", refusing self-links and cycles. Returns whether it was new.
    fn link(&mut self, from: NodeId, to: NodeId) -> Result<bool, String> {
        if from == to {
            return Err(format!("{} cannot depend on itself", self.name(from)));
        }
        if self.edges.iter().any(|e| e.from == from && e.to == to) {
            return Ok(false);
        }
        if layout::creates_cycle(&self.pairs(), from, to) {
            return Err(format!(
                "{} depending on {} would create a dependency cycle ({} already depends on {}, directly or not)",
                self.name(from),
                self.name(to),
                self.name(to),
                self.name(from)
            ));
        }
        self.edges.push(EdgeRec { from, to, type_id: EdgeTypeId::DEPENDS_ON });
        self.writes.push(Write::Link { from, to, type_id: EdgeTypeId::DEPENDS_ON });
        Ok(true)
    }

    fn apply(&mut self, op: &Op) -> Result<Value, String> {
        let result = match op {
            Op::CreateNode { title, depends_on, dependants, x, y } => {
                let title = normalize_title(title)?;
                let deps = self.resolve_all(depends_on)?;
                let dependants = self.resolve_all(dependants)?;
                let id = NodeId::new();

                // Where it goes: beside what it depends on, else before what depends on it,
                // else below everything.
                let pos = layout::derive_positions(&self.places);
                let (parent, at) = if let Some(&p) = deps.first() {
                    (Some(p), Vec2::ZERO)
                } else if let Some(d) = dependants.first().and_then(|d| pos.get(d)) {
                    (None, *d - Vec2::new(NODE_WIDTH + layout::FLOW_GAP, 0))
                } else if let (Some(x), Some(y)) = (x, y) {
                    (None, Vec2::new(*x, *y))
                } else if pos.is_empty() {
                    (None, Vec2::ZERO)
                } else {
                    let left = pos.values().map(|p| p.x).min().unwrap_or(0);
                    let bottom = pos.values().map(|p| p.y).max().unwrap_or(0);
                    (None, Vec2::new(left, bottom + NODE_HEIGHT + 2 * layout::MARGIN))
                };
                let changes = layout::add_node(&self.places, &self.pairs(), id, parent, at);
                let own = changes.iter().find(|(n, _)| *n == id).map(|(_, p)| *p).expect("add_node places the new node");
                self.writes.push(Write::CreateNode { id, title: title.clone(), place: own });
                self.nodes.insert(id, NodeRec { id, key: None, title: title.clone(), place: own, note: String::new() });
                self.created.push(Some(id)); // so the links below, and later ops, can name it
                self.set_places(changes, Some(id));

                for dep in deps {
                    self.link(id, dep)?;
                }
                for d in dependants {
                    self.link(d, id)?;
                }
                json!({ "op": "create_node", "id": id.0, "title": title })
            }
            Op::RenameNode { node, title } => {
                let id = self.resolve(node)?;
                let title = normalize_title(title)?;
                self.writes.push(Write::SetTitle { id, title: title.clone() });
                if let Some(n) = self.nodes.get_mut(&id) {
                    n.title = title.clone();
                }
                json!({ "op": "rename_node", "id": id.0, "title": title })
            }
            Op::DeleteNode { node } => {
                let id = self.resolve(node)?;
                let name = self.name(id);
                let triples: Vec<_> = self.edges.iter().map(|e| (e.from, e.to, e.type_id)).collect();
                let bridges = layout::bridge_edges(&triples, id);
                let remaining: Vec<_> =
                    triples.iter().filter(|t| t.0 != id && t.1 != id).chain(bridges.iter()).copied().collect();
                let pairs: Vec<_> = remaining.iter().map(|t| (t.0, t.1)).collect();
                // Re-anchor while the node still exists, so no position changes.
                let changes: Vec<_> =
                    layout::repair_anchors(&self.places, &pairs).into_iter().filter(|(n, _)| *n != id).collect();
                for &(from, to, type_id) in &bridges {
                    self.writes.push(Write::Link { from, to, type_id });
                }
                self.set_places(changes, None);
                self.writes.push(Write::DeleteNode { id });
                self.places.remove(&id);
                self.nodes.remove(&id);
                self.edges = remaining.into_iter().map(|(from, to, type_id)| EdgeRec { from, to, type_id }).collect();
                json!({ "op": "delete_node", "deleted": name, "joined": bridges.len() })
            }
            Op::Link { from, to } => {
                let (from, to) = (self.resolve(from)?, self.resolve(to)?);
                let added = self.link(from, to)?;
                json!({ "op": "link", "from": from.0, "to": to.0, "already_linked": !added })
            }
            Op::Unlink { from, to } => {
                let (from, to) = (self.resolve(from)?, self.resolve(to)?);
                if self.conflicted.iter().any(|e| e.from == from && e.to == to) {
                    self.conflicted.retain(|e| !(e.from == from && e.to == to));
                    self.writes.push(Write::Unlink { from, to });
                    return Ok(json!({ "op": "unlink", "from": from.0, "to": to.0, "was_conflicted": true }));
                }
                if !self.edges.iter().any(|e| e.from == from && e.to == to) {
                    return Err(format!("{} does not depend on {}", self.name(from), self.name(to)));
                }
                self.edges.retain(|e| !(e.from == from && e.to == to));
                self.writes.push(Write::Unlink { from, to });
                // Nodes that were anchored through this edge are re-anchored without moving.
                let changes = layout::repair_anchors(&self.places, &self.pairs());
                self.set_places(changes, None);
                json!({ "op": "unlink", "from": from.0, "to": to.0 })
            }
            Op::MoveNode { node, x, y, with_subtree } => {
                let id = self.resolve(node)?;
                let changes = layout::drop_node(&self.places, &self.pairs(), id, Vec2::new(*x, *y), *with_subtree);
                let moved = changes.len();
                self.set_places(changes, None);
                json!({ "op": "move_node", "id": id.0, "places_changed": moved })
            }
            Op::SetNote { node, body } => {
                let id = self.resolve(node)?;
                check_note(body)?;
                self.writes.push(Write::SetNote { id, body: body.clone() });
                if let Some(n) = self.nodes.get_mut(&id) {
                    n.note = body.clone();
                }
                json!({ "op": "set_note", "id": id.0 })
            }
            Op::AppendNote { node, text } => {
                let id = self.resolve(node)?;
                if text.trim().is_empty() {
                    return Err("nothing to append".into());
                }
                let whole = match self.nodes.get(&id).map(|n| n.note.as_str()).filter(|n| !n.is_empty()) {
                    Some(old) => format!("{old}\n\n{text}"),
                    None => text.clone(),
                };
                check_note(&whole)?;
                self.writes.push(Write::AppendNote { id, text: text.clone() });
                if let Some(n) = self.nodes.get_mut(&id) {
                    n.note = whole;
                }
                json!({ "op": "append_note", "id": id.0 })
            }
        };
        if !matches!(op, Op::CreateNode { .. }) {
            self.created.push(None);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::Graph;
    use serde_json::json;
    use waypoint_model::Batch;

    /// Plan and apply a batch given as JSON, numbering new nodes afterwards, as the server does.
    fn run(g: &Graph, ops: Value) -> Result<Vec<Value>, OpError> {
        g.assign_keys();
        let batch: Batch = serde_json::from_value(json!({ "ops": ops })).expect("valid batch");
        let plan = plan(&g.snapshot(), &batch.ops)?;
        g.apply_writes(&plan.writes);
        g.assign_keys();
        Ok(plan.results)
    }

    fn pos_of(g: &Graph, title: &str) -> Vec2 {
        let snap = g.snapshot();
        let id = snap.nodes.values().find(|n| n.title == title).unwrap_or_else(|| panic!("no node {title}")).id;
        layout::derive_positions(&snap.places())[&id]
    }

    fn links(g: &Graph) -> Vec<String> {
        let snap = g.snapshot();
        let t = |id: NodeId| snap.nodes[&id].title.clone();
        let mut v: Vec<_> = snap.edges.iter().map(|e| format!("{}>{}", t(e.from), t(e.to))).collect();
        v.sort();
        v
    }

    fn all_positions(g: &Graph) -> BTreeMap<String, Vec2> {
        let snap = g.snapshot();
        let pos = layout::derive_positions(&snap.places());
        snap.nodes.values().map(|n| (n.title.clone(), pos[&n.id])).collect()
    }

    #[test]
    fn a_batch_can_create_nodes_and_link_them_with_dollar_references() {
        let g = Graph::new();
        let results = run(
            &g,
            json!([
                { "op": "create_node", "title": "design" },
                { "op": "create_node", "title": "build", "depends_on": ["$0"] },
                { "op": "create_node", "title": "ship", "depends_on": ["$1"] },
                { "op": "link", "from": "$2", "to": "$0" },
            ]),
        )
        .unwrap();
        assert_eq!(results.len(), 4);
        assert_eq!(links(&g), vec!["build>design", "ship>build", "ship>design"]);
        // Keys are assigned in order, and an agent can use them from now on.
        let keys: Vec<_> = (1..=3).map(|k| format!("#{k}")).collect();
        let titles: Vec<_> = keys
            .iter()
            .map(|k| {
                let snap = g.snapshot();
                let id = resolve_ref(&snap.nodes, &[], k).unwrap();
                snap.nodes[&id].title.clone()
            })
            .collect();
        assert_eq!(titles, vec!["design", "build", "ship"]);
    }

    #[test]
    fn a_dependant_is_placed_to_the_right_of_what_it_depends_on() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b", "depends_on": ["$0"] }])).unwrap();
        let (a, b) = (pos_of(&g, "a"), pos_of(&g, "b"));
        assert!(b.x >= a.x + NODE_WIDTH && b.y == a.y, "a at {a:?}, b at {b:?}");
    }

    #[test]
    fn nodes_created_with_dependants_go_before_them() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "late" }])).unwrap();
        run(&g, json!([{ "op": "create_node", "title": "early", "dependants": ["#1"] }])).unwrap();
        assert!(pos_of(&g, "early").x < pos_of(&g, "late").x);
        assert_eq!(links(&g), vec!["late>early"]);
    }

    #[test]
    fn creating_many_nodes_never_leaves_them_overlapping() {
        let g = Graph::new();
        let ops: Vec<_> = (0..12).map(|i| json!({ "op": "create_node", "title": format!("n{i}"), "depends_on": if i == 0 { vec![] } else { vec!["$0".to_string()] } })).collect();
        run(&g, Value::Array(ops)).unwrap();
        let pos: Vec<_> = all_positions(&g).into_values().collect();
        for (i, a) in pos.iter().enumerate() {
            for b in &pos[i + 1..] {
                let apart = (a.x - b.x).abs() >= NODE_WIDTH || (a.y - b.y).abs() >= NODE_HEIGHT;
                assert!(apart, "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn a_failing_op_leaves_the_graph_untouched() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b", "depends_on": ["$0"] }])).unwrap();
        let before = (links(&g), all_positions(&g));
        // The first two ops are fine; the cycle in the third must undo everything.
        let err = run(
            &g,
            json!([
                { "op": "create_node", "title": "c" },
                { "op": "rename_node", "node": "#1", "title": "renamed" },
                { "op": "link", "from": "#1", "to": "#2" },
            ]),
        )
        .unwrap_err();
        assert_eq!(err.op_index, 2);
        assert!(err.message.contains("cycle"), "{}", err.message);
        assert_eq!((links(&g), all_positions(&g)), before);
        assert_eq!(g.snapshot().nodes.len(), 2, "the create_node from the same batch was rolled back too");
    }

    #[test]
    fn errors_say_what_is_wrong() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }])).unwrap();
        let msg = |ops: Value| run(&g, ops).unwrap_err().message;
        assert!(msg(json!([{ "op": "rename_node", "node": "#9", "title": "x" }])).contains("no node with key #9"));
        assert!(msg(json!([{ "op": "rename_node", "node": "banana", "title": "x" }])).contains("not a node reference"));
        assert!(msg(json!([{ "op": "link", "from": "#1", "to": "#1" }])).contains("itself"));
        assert!(msg(json!([{ "op": "create_node", "title": "   " }])).contains("empty"));
        assert!(msg(json!([{ "op": "link", "from": "$3", "to": "#1" }])).contains("earlier in this batch"));
        assert!(msg(json!([{ "op": "unlink", "from": "#1", "to": "#1" }])).contains("does not depend"));
    }

    #[test]
    fn links_are_identified_by_their_pair() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b" }])).unwrap();
        let r = run(&g, json!([{ "op": "link", "from": "#1", "to": "#2" }, { "op": "link", "from": "#1", "to": "#2" }])).unwrap();
        assert_eq!(r[0]["already_linked"], false);
        assert_eq!(r[1]["already_linked"], true, "repeating a link is harmless");
        assert_eq!(links(&g), vec!["a>b"]);
        run(&g, json!([{ "op": "unlink", "from": "#1", "to": "#2" }])).unwrap();
        assert!(links(&g).is_empty());
    }

    #[test]
    fn deleting_the_middle_of_a_chain_joins_it_and_moves_nothing() {
        let g = Graph::new();
        run(
            &g,
            json!([
                { "op": "create_node", "title": "a" },
                { "op": "create_node", "title": "b", "depends_on": ["$0"] },
                { "op": "create_node", "title": "c", "depends_on": ["$1"] },
            ]),
        )
        .unwrap();
        let before = all_positions(&g);
        let r = run(&g, json!([{ "op": "delete_node", "node": "#2" }])).unwrap();
        assert_eq!(r[0]["joined"], 1);
        assert_eq!(links(&g), vec!["c>a"]);
        let after = all_positions(&g);
        assert_eq!(after.get("a"), before.get("a"));
        assert_eq!(after.get("c"), before.get("c"), "c was anchored to b; it must not move");
        assert!(!after.contains_key("b"));
        // The key #2 is retired: the next node is #4.
        run(&g, json!([{ "op": "create_node", "title": "d" }])).unwrap();
        let snap = g.snapshot();
        assert_eq!(snap.nodes.values().find(|n| n.title == "d").unwrap().key, Some(4));
    }

    #[test]
    fn unlinking_keeps_every_node_where_it_is() {
        let g = Graph::new();
        run(
            &g,
            json!([
                { "op": "create_node", "title": "a" },
                { "op": "create_node", "title": "b", "depends_on": ["$0"] },
                { "op": "create_node", "title": "c", "depends_on": ["$1"] },
            ]),
        )
        .unwrap();
        let before = all_positions(&g);
        run(&g, json!([{ "op": "unlink", "from": "#2", "to": "#1" }])).unwrap();
        assert_eq!(all_positions(&g), before);
        // After the unlink, b is no longer anchored to a: moving a leaves b alone.
        run(&g, json!([{ "op": "move_node", "node": "#1", "x": 0, "y": -500 }])).unwrap();
        assert_eq!(pos_of(&g, "b"), before["b"]);
    }

    #[test]
    fn a_conflicted_link_can_be_removed_and_is_reported() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b", "depends_on": ["$0"] }])).unwrap();
        let snap = g.snapshot();
        let (a, b) = (snap.nodes.values().find(|n| n.title == "a").unwrap().id, snap.nodes.values().find(|n| n.title == "b").unwrap().id);
        // A second client's link a -> b arrives and is set aside.
        g.create_edge(a, b, waypoint_model::EdgeTypeId::DEPENDS_ON);
        g.apply_writes(&crate::repair::plan(&g.snapshot()));
        let reported = crate::api::conflicts(&g.snapshot());
        assert_eq!(reported["conflicts"].as_array().unwrap().len(), 1);
        assert_eq!(crate::api::summary(&g.snapshot())["conflicts"], 1);

        let r = run(&g, json!([{ "op": "unlink", "from": reported["conflicts"][0]["from"], "to": reported["conflicts"][0]["to"] }])).unwrap();
        assert_eq!(r[0]["was_conflicted"], true);
        assert!(g.snapshot().conflicted.is_empty());
        assert_eq!(links(&g).len(), 1, "the other link of the pair is untouched");
    }

    #[test]
    fn renaming_changes_the_title_but_not_the_key() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "old" }])).unwrap();
        run(&g, json!([{ "op": "rename_node", "node": "#1", "title": "  new   name " }])).unwrap();
        let snap = g.snapshot();
        let n = snap.nodes.values().next().unwrap();
        assert_eq!((n.title.as_str(), n.key), ("new name", Some(1)));
    }

    #[test]
    fn moving_a_node_onto_another_pushes_that_one_away() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b" }])).unwrap();
        let b = pos_of(&g, "b");
        run(&g, json!([{ "op": "move_node", "node": "#1", "x": b.x, "y": b.y }])).unwrap();
        assert_eq!(pos_of(&g, "a"), b, "the moved node lands where it was put");
        assert_ne!(pos_of(&g, "b"), b, "and the other makes room");
    }

    #[test]
    fn a_node_can_be_created_at_an_explicit_position() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "here", "x": 1000, "y": 500 }])).unwrap();
        assert_eq!(pos_of(&g, "here"), Vec2::new(1000, 500));
    }
    #[test]
    fn batches_are_size_limited() {
        let g = Graph::new();
        let ops = Value::Array((0..=MAX_OPS).map(|i| json!({ "op": "create_node", "title": format!("n{i}") })).collect());
        assert!(run(&g, ops).unwrap_err().message.contains("at most"));
    }

    fn note_of(g: &Graph, title: &str) -> String {
        g.snapshot().nodes.values().find(|n| n.title == title).unwrap().note.clone()
    }

    #[test]
    fn a_node_has_one_note_that_can_be_set_and_appended_to() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }])).unwrap();
        assert_eq!(note_of(&g, "a"), "");
        run(&g, json!([{ "op": "set_note", "node": "#1", "body": "first, é" }])).unwrap();
        assert_eq!(note_of(&g, "a"), "first, é");
        run(&g, json!([{ "op": "set_note", "node": "#1", "body": "first, é and more" }])).unwrap();
        run(&g, json!([{ "op": "append_note", "node": "#1", "text": "tail" }])).unwrap();
        assert_eq!(note_of(&g, "a"), "first, é and more\n\ntail");
        run(&g, json!([{ "op": "set_note", "node": "#1", "body": "" }])).unwrap();
        assert_eq!(note_of(&g, "a"), "", "clearing is allowed");
        run(&g, json!([{ "op": "append_note", "node": "#1", "text": "x" }])).unwrap();
        assert_eq!(note_of(&g, "a"), "x", "no blank line before the first text");
        assert!(run(&g, json!([{ "op": "set_note", "node": "#9", "body": "x" }])).is_err(), "no such node");
        assert!(run(&g, json!([{ "op": "append_note", "node": "#1", "text": " " }])).is_err());
    }

    #[test]
    fn concurrent_edits_to_different_parts_of_a_note_both_survive() {
        let a = Graph::new();
        run(&a, json!([{ "op": "create_node", "title": "n" }, { "op": "set_note", "node": "$0", "body": "alpha beta gamma" }])).unwrap();
        let b = Graph::new();
        b.apply_update(&a.encode_state()).unwrap();
        run(&a, json!([{ "op": "set_note", "node": "#1", "body": "ALPHA beta gamma" }])).unwrap();
        run(&b, json!([{ "op": "set_note", "node": "#1", "body": "alpha beta GAMMA" }])).unwrap();
        a.apply_update(&b.encode_state()).unwrap();
        assert_eq!(note_of(&a, "n"), "ALPHA beta GAMMA");
    }

    #[test]
    fn dot_lists_nodes_and_links() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "say \"hi\"" }, { "op": "create_node", "title": "b", "depends_on": ["$0"] }])).unwrap();
        let dot = crate::api::dot(&g.snapshot());
        assert!(dot.starts_with("digraph waypoint {"));
        assert!(dot.contains("label=\"#1 say \\\"hi\\\"\""), "{dot}");
        assert_eq!(dot.matches("\" -> \"").count(), 1, "{dot}");
    }
}
