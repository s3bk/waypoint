//! Agent operations: plan a batch against a snapshot of the graph.
//!
//! Planning reads a [`Snapshot`], applies each op to an in-memory working copy
//! (using the layout crate for placement, making room, re-anchoring and joining
//! links), and records the resulting [`Write`]s. Nothing touches the document
//! until the whole batch has planned successfully, so a batch is atomic: if any
//! op fails, none of it is applied.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use uuid::Uuid;
use waypoint_layout as layout;
use waypoint_model::{
    EdgeTypeId, NODE_HEIGHT, NODE_WIDTH, NodeId, Op, Place, Vec2,
    style::{self, EdgeType, Kind, NodeType, Tag},
};

use crate::graph::{EdgeRec, NodeRec, Snapshot, TagTarget, Write, edge_diff, node_diff, tag_diff};

pub const MAX_OPS: usize = 100;
const MAX_TITLE: usize = 200;
const MAX_NOTE: usize = 20_000;
/// Types per registry.
const MAX_TYPES: usize = 64;

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
    /// Set aside by the server (a link to itself); they can only be removed.
    conflicted: Vec<EdgeRec>,
    node_types: Vec<NodeType>,
    edge_types: Vec<EdgeType>,
    tags: Vec<Tag>,
    edge_tags: BTreeMap<(NodeId, NodeId), BTreeSet<uuid::Uuid>>,
    background: String,
    writes: Vec<Write>,
    /// The node each op created, by op index, for `$N` references.
    created: Vec<Option<NodeId>>,
    /// A batch that adds many nodes without saying where lays them out together at the end,
    /// instead of one by one: `Some(origin)` is where the drawing's top-left goes.
    defer_to: Option<Vec2>,
    /// The nodes waiting for that layout.
    deferred: Vec<NodeId>,
}

/// A batch creating at least this many nodes without positions gets a layered layout.
pub const AUTO_LAYOUT_MIN: usize = 3;

/// Farthest a layout may put a node from the origin.
const MAX_COORD: i32 = 1_000_000;

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
        node_types: snapshot.node_types.clone(),
        edge_types: snapshot.edge_types.clone(),
        tags: snapshot.tags.clone(),
        edge_tags: snapshot.edge_tags.clone(),
        background: snapshot.background.clone(),
        writes: Vec::new(),
        created: Vec::with_capacity(ops.len()),
        defer_to: None,
        deferred: Vec::new(),
    };
    let unplaced = ops.iter().filter(|op| matches!(op, Op::CreateNode { x: None, y: None, .. })).count();
    if unplaced >= AUTO_LAYOUT_MIN {
        // A new drawing goes below everything that is there, or at the origin of an empty graph.
        let pos = layout::derive_positions(&work.places);
        work.defer_to = Some(match (pos.values().map(|p| p.x).min(), pos.values().map(|p| p.y).max()) {
            (Some(left), Some(bottom)) => Vec2::new(left, bottom + NODE_HEIGHT + 2 * layout::MARGIN),
            _ => Vec2::ZERO,
        });
    }
    let mut results = Vec::with_capacity(ops.len());
    for (op_index, op) in ops.iter().enumerate() {
        let result = work.apply(op).map_err(|message| OpError { op_index, message })?;
        results.push(result);
    }
    work.lay_out_deferred();
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

    /// Lay out the nodes of a big batch together: layers left to right from destinations to sources, then fewer crossings.
    fn lay_out_deferred(&mut self) {
        let Some(origin) = self.defer_to else { return };
        let nodes = std::mem::take(&mut self.deferred);
        let inside: BTreeSet<NodeId> = nodes.iter().copied().collect();
        let pairs: Vec<_> = self.pairs().into_iter().filter(|(f, t)| inside.contains(f) && inside.contains(t)).collect();
        let positions = layout::layered(&nodes, &pairs);
        let changes = layout::as_places(&positions, &pairs, origin);
        self.set_places(changes, None);
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

    fn node_type_named(&self, name: &str) -> Result<&NodeType, String> {
        let n = style::normalize_name(name)?;
        self.node_types.iter().find(|t| t.name == n).ok_or_else(|| {
            let names: Vec<_> = self.node_types.iter().map(|t| t.name.as_str()).collect();
            format!("no node type {n:?}; the types are {} (create one with upsert_node_type)", names.join(", "))
        })
    }

    fn tag_named(&self, name: &str) -> Result<&Tag, String> {
        let n = style::normalize_name(name)?;
        self.tags.iter().find(|t| t.name == n).ok_or_else(|| {
            let names: Vec<_> = self.tags.iter().map(|t| t.name.as_str()).collect();
            format!("no tag {n:?}; the tags are {} (create one with upsert_tag)", if names.is_empty() { "none yet".to_owned() } else { names.join(", ") })
        })
    }

    fn tag_names(&self, ids: &BTreeSet<uuid::Uuid>) -> Vec<String> {
        self.tags.iter().filter(|t| ids.contains(&t.id)).map(|t| t.name.clone()).collect()
    }

    fn edge_type_named(&self, name: &str) -> Result<&EdgeType, String> {
        let n = style::normalize_name(name)?;
        // The default edge type used to be called "depends_on"; graphs made then still have it under that name.
        let by_name = self.edge_types.iter().find(|t| t.name == n);
        let legacy = (n == "depends_on").then(|| self.edge_types.iter().find(|t| t.id == style::DEFAULT_EDGE_TYPE)).flatten();
        by_name.or(legacy).ok_or_else(|| {
            let names: Vec<_> = self.edge_types.iter().map(|t| t.name.as_str()).collect();
            format!("no edge type {n:?}; the types are {} (create one with upsert_edge_type)", names.join(", "))
        })
    }

    /// Give `node` a type; the default type is stored as no type.
    fn set_node_type(&mut self, node: NodeId, type_id: uuid::Uuid) {
        let stored = (type_id != style::DEFAULT_NODE_TYPE).then_some(type_id);
        self.writes.push(Write::SetNodeType { id: node, type_id: stored });
        if let Some(n) = self.nodes.get_mut(&node) {
            n.type_id = stored;
        }
    }

    /// Add "from is the source, to the destination", refusing self-links (links may run in any direction, and loop back). Returns whether it was new.
    /// Linking an existing pair with another `type_id` changes its type.
    fn link(&mut self, from: NodeId, to: NodeId, type_id: Option<EdgeTypeId>) -> Result<bool, String> {
        if from == to {
            return Err(format!("{} cannot link to itself", self.name(from)));
        }
        if let Some(existing) = self.edges.iter_mut().find(|e| e.from == from && e.to == to) {
            if let Some(t) = type_id.filter(|t| *t != existing.type_id) {
                existing.type_id = t;
                self.writes.push(Write::Link { from, to, type_id: t });
            }
            return Ok(false);
        }
        let type_id = type_id.unwrap_or(EdgeTypeId::DEFAULT);
        self.edges.push(EdgeRec { from, to, type_id });
        self.writes.push(Write::Link { from, to, type_id });
        Ok(true)
    }

    fn apply(&mut self, op: &Op) -> Result<Value, String> {
        let result = match op {
            Op::CreateNode { title, destinations, sources, x, y, node_type } => {
                let title = normalize_title(title)?;
                let node_type = node_type.as_deref().map(|n| self.node_type_named(n).map(|t| t.id)).transpose()?;
                let deps = self.resolve_all(destinations)?;
                let sources = self.resolve_all(sources)?;
                let id = NodeId::new();

                // Where it goes: beside its destinations, else before its sources,
                // else below everything.
                let pos = layout::derive_positions(&self.places);
                let (parent, at) = if let Some(&p) = deps.first() {
                    (Some(p), Vec2::ZERO)
                } else if let Some(d) = sources.first().and_then(|d| pos.get(d)) {
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
                if let (Some(origin), None, None) = (self.defer_to, x, y) {
                    // Laid out with the rest of the batch at the end; until then it waits where nothing else is.
                    let own = Place::at_origin(origin);
                    self.writes.push(Write::CreateNode { id, title: title.clone(), place: own });
                    self.nodes.insert(id, NodeRec { id, key: None, title: title.clone(), place: own, note: String::new(), type_id: None, tags: Default::default() });
                    self.places.insert(id, own);
                    self.deferred.push(id);
                    self.created.push(Some(id));
                    if let Some(t) = node_type {
                        self.set_node_type(id, t);
                    }
                    for dep in deps {
                        self.link(id, dep, None)?;
                    }
                    for d in sources {
                        self.link(d, id, None)?;
                    }
                    return Ok(json!({ "op": "create_node", "id": id.0, "title": title }));
                }
                let changes = layout::add_node(&self.places, &self.pairs(), id, parent, at);
                let own = changes.iter().find(|(n, _)| *n == id).map(|(_, p)| *p).expect("add_node places the new node");
                self.writes.push(Write::CreateNode { id, title: title.clone(), place: own });
                self.nodes.insert(id, NodeRec { id, key: None, title: title.clone(), place: own, note: String::new(), type_id: None, tags: Default::default() });
                self.created.push(Some(id)); // so the links below, and later ops, can name it
                self.set_places(changes, Some(id));
                if let Some(t) = node_type {
                    self.set_node_type(id, t);
                }

                for dep in deps {
                    self.link(id, dep, None)?;
                }
                for d in sources {
                    self.link(d, id, None)?;
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
                self.edge_tags.retain(|(f, t), _| *f != id && *t != id);
                self.edges = remaining.into_iter().map(|(from, to, type_id)| EdgeRec { from, to, type_id }).collect();
                json!({ "op": "delete_node", "deleted": name, "joined": bridges.len() })
            }
            Op::Link { from, to, edge_type } => {
                let (from, to) = (self.resolve(from)?, self.resolve(to)?);
                let type_id = edge_type.as_deref().map(|n| self.edge_type_named(n).map(|t| EdgeTypeId(t.id))).transpose()?;
                let added = self.link(from, to, type_id)?;
                json!({ "op": "link", "from": from.0, "to": to.0, "already_linked": !added })
            }
            Op::SetNodeType { node, node_type } => {
                let id = self.resolve(node)?;
                let t = self.node_type_named(node_type)?.clone();
                self.set_node_type(id, t.id);
                json!({ "op": "set_node_type", "id": id.0, "node_type": t.name })
            }
            Op::UpsertNodeType(spec) => {
                let name = style::normalize_name(&spec.name)?;
                let old = self.node_types.iter().find(|t| t.name == name).cloned();
                let new = match &old {
                    Some(old) => style::apply_node(old.clone(), spec)?,
                    None if self.node_types.len() >= MAX_TYPES => return Err(format!("a graph may have at most {MAX_TYPES} node types")),
                    None => style::new_node(spec)?,
                };
                if old.as_ref().is_none_or(|o| o.name != new.name) && self.node_types.iter().any(|t| t.name == new.name && t.id != new.id) {
                    return Err(format!("there is already a node type named {:?}", new.name));
                }
                let fields = node_diff(old.as_ref(), &new);
                if !fields.is_empty() {
                    self.writes.push(Write::UpsertType { kind: Kind::Node, id: new.id, fields });
                }
                self.node_types.retain(|t| t.id != new.id);
                self.node_types.push(new.clone());
                json!({ "op": "upsert_node_type", "created": old.is_none(), "type": new })
            }
            Op::UpsertEdgeType(spec) => {
                let name = style::normalize_name(&spec.name)?;
                let old = self.edge_types.iter().find(|t| t.name == name).cloned();
                let new = match &old {
                    Some(old) => style::apply_edge(old.clone(), spec, &self.background)?,
                    None if self.edge_types.len() >= MAX_TYPES => return Err(format!("a graph may have at most {MAX_TYPES} edge types")),
                    None => style::new_edge(spec, &self.background)?,
                };
                if old.as_ref().is_none_or(|o| o.name != new.name) && self.edge_types.iter().any(|t| t.name == new.name && t.id != new.id) {
                    return Err(format!("there is already an edge type named {:?}", new.name));
                }
                let fields = edge_diff(old.as_ref(), &new);
                if !fields.is_empty() {
                    self.writes.push(Write::UpsertType { kind: Kind::Edge, id: new.id, fields });
                }
                self.edge_types.retain(|t| t.id != new.id);
                self.edge_types.push(new.clone());
                json!({ "op": "upsert_edge_type", "created": old.is_none(), "type": new })
            }
            Op::UpsertTag(spec) => {
                let name = style::normalize_name(&spec.name)?;
                let old = self.tags.iter().find(|t| t.name == name).cloned();
                let new = match &old {
                    Some(old) => style::apply_tag(old.clone(), spec)?,
                    None if self.tags.len() >= MAX_TYPES => return Err(format!("a graph may have at most {MAX_TYPES} tags")),
                    None => style::new_tag(spec)?,
                };
                if old.as_ref().is_none_or(|o| o.name != new.name) && self.tags.iter().any(|t| t.name == new.name && t.id != new.id) {
                    return Err(format!("there is already a tag named {:?}", new.name));
                }
                let fields = tag_diff(old.as_ref(), &new);
                if !fields.is_empty() {
                    self.writes.push(Write::UpsertType { kind: Kind::Tag, id: new.id, fields });
                }
                self.tags.retain(|t| t.id != new.id);
                self.tags.push(new.clone());
                json!({ "op": "upsert_tag", "created": old.is_none(), "tag": new })
            }
            Op::RemoveTag { name } => {
                let t = self.tag_named(name)?.clone();
                self.writes.push(Write::RemoveType { kind: Kind::Tag, id: t.id });
                self.tags.retain(|x| x.id != t.id);
                let users = self.nodes.values().filter(|n| n.tags.contains(&t.id)).count() + self.edge_tags.values().filter(|s| s.contains(&t.id)).count();
                json!({ "op": "remove_tag", "removed": t.name, "was_on": users })
            }
            Op::SetTags { node, from, to, add, remove } => {
                let adding = add.iter().map(|n| self.tag_named(n).map(|t| t.id)).collect::<Result<Vec<_>, _>>()?;
                let removing = remove.iter().map(|n| self.tag_named(n).map(|t| t.id)).collect::<Result<Vec<_>, _>>()?;
                if let Some(t) = adding.iter().find(|t| removing.contains(t)) {
                    return Err(format!("tag {:?} is in both `add` and `remove`", self.tags.iter().find(|x| x.id == *t).map(|x| x.name.as_str()).unwrap_or("")));
                }
                let changes: Vec<(uuid::Uuid, bool)> = adding.into_iter().map(|t| (t, true)).chain(removing.into_iter().map(|t| (t, false))).collect();
                match (node, from, to) {
                    (Some(node), None, None) => {
                        let id = self.resolve(node)?;
                        let target = TagTarget::Node(id);
                        let have = &mut self.nodes.get_mut(&id).ok_or("no such node")?.tags;
                        for &(tag, on) in &changes {
                            if on { have.insert(tag) } else { have.remove(&tag) };
                            self.writes.push(Write::SetTag { target, tag, on });
                        }
                        json!({ "op": "set_tags", "id": id.0, "tags": self.tag_names(&self.nodes[&id].tags) })
                    }
                    (None, Some(from), Some(to)) => {
                        let (from, to) = (self.resolve(from)?, self.resolve(to)?);
                        if !self.edges.iter().any(|e| e.from == from && e.to == to) {
                            return Err(format!("{} is not linked to {}", self.name(from), self.name(to)));
                        }
                        let target = TagTarget::Link(from, to);
                        let have = self.edge_tags.entry((from, to)).or_default();
                        for &(tag, on) in &changes {
                            if on { have.insert(tag) } else { have.remove(&tag) };
                            self.writes.push(Write::SetTag { target, tag, on });
                        }
                        json!({ "op": "set_tags", "from": from.0, "to": to.0, "tags": self.tag_names(&self.edge_tags[&(from, to)]) })
                    }
                    _ => return Err("give `node`, or `from` and `to` for a link".into()),
                }
            }
            Op::RemoveNodeType { name } => {
                let t = self.node_type_named(name)?.clone();
                if t.id == style::DEFAULT_NODE_TYPE {
                    return Err("the default node type cannot be removed (change its style instead)".into());
                }
                self.writes.push(Write::RemoveType { kind: Kind::Node, id: t.id });
                self.node_types.retain(|x| x.id != t.id);
                let users = self.nodes.values().filter(|n| n.type_id == Some(t.id)).count();
                json!({ "op": "remove_node_type", "removed": t.name, "nodes_now_default": users })
            }
            Op::RemoveEdgeType { name } => {
                let t = self.edge_type_named(name)?.clone();
                if t.id == style::DEFAULT_EDGE_TYPE {
                    return Err("the default edge type cannot be removed (change its style instead)".into());
                }
                self.writes.push(Write::RemoveType { kind: Kind::Edge, id: t.id });
                self.edge_types.retain(|x| x.id != t.id);
                let users = self.edges.iter().filter(|e| e.type_id.0 == t.id).count();
                json!({ "op": "remove_edge_type", "removed": t.name, "edges_now_default": users })
            }
            Op::Unlink { from, to } => {
                let (from, to) = (self.resolve(from)?, self.resolve(to)?);
                if self.conflicted.iter().any(|e| e.from == from && e.to == to) {
                    self.conflicted.retain(|e| !(e.from == from && e.to == to));
                    self.writes.push(Write::Unlink { from, to });
                    return Ok(json!({ "op": "unlink", "from": from.0, "to": to.0, "was_conflicted": true }));
                }
                if !self.edges.iter().any(|e| e.from == from && e.to == to) {
                    return Err(format!("{} is not linked to {}", self.name(from), self.name(to)));
                }
                self.edges.retain(|e| !(e.from == from && e.to == to));
                self.edge_tags.remove(&(from, to));
                self.writes.push(Write::Unlink { from, to });
                // Nodes that were anchored through this edge are re-anchored without moving.
                let changes = layout::repair_anchors(&self.places, &self.pairs());
                self.set_places(changes, None);
                json!({ "op": "unlink", "from": from.0, "to": to.0 })
            }
            Op::MoveNode { node, x, y, with_destinations } => {
                let id = self.resolve(node)?;
                let changes = layout::drop_node(&self.places, &self.pairs(), id, Vec2::new(*x, *y), *with_destinations);
                let moved = changes.len();
                self.set_places(changes, None);
                json!({ "op": "move_node", "id": id.0, "places_changed": moved })
            }
            Op::Tidy { aspect } => {
                if self.nodes.is_empty() {
                    return Ok(json!({ "op": "tidy", "nodes": 0 }));
                }
                let kinds: BTreeSet<Option<uuid::Uuid>> = self.nodes.values().map(|n| n.type_id).collect();
                let kinds: Vec<_> = kinds.into_iter().collect();
                let nodes: Vec<(NodeId, u32)> = self
                    .nodes
                    .values()
                    .map(|n| (n.id, kinds.iter().position(|k| *k == n.type_id).unwrap_or(0) as u32))
                    .collect();
                // Links with a label need room for its text.
                let pairs = self.pairs();
                let labels: layout::Labels = self
                    .edges
                    .iter()
                    .filter_map(|e| {
                        let chars = self.edge_types.iter().find(|t| t.id == e.type_id.0)?.label.chars().count();
                        (chars > 0).then(|| ((e.from, e.to), chars as f64 * layout::LABEL_CHAR_WIDTH))
                    })
                    .collect();
                let positions = layout::tidy(&nodes, &pairs, aspect.unwrap_or(16.0 / 9.0), &labels);
                let changes = layout::as_places(&positions, &pairs, Vec2::ZERO);
                let count = changes.len();
                self.set_places(changes, None);
                json!({ "op": "tidy", "nodes": count })
            }
            Op::Layout { positions } => {
                let mut placed: BTreeMap<NodeId, Vec2> = BTreeMap::new();
                for p in positions {
                    let id = self.resolve(&p.node)?;
                    if p.x.abs() > MAX_COORD || p.y.abs() > MAX_COORD {
                        return Err(format!("positions must stay within {MAX_COORD} of the origin"));
                    }
                    placed.insert(id, Vec2::new(p.x, p.y));
                }
                if let Some(missing) = self.nodes.keys().find(|id| !placed.contains_key(id)) {
                    return Err(format!("the layout lacks {} (the graph changed since it was made?)", self.name(*missing)));
                }
                let pairs = self.pairs();
                let changes = layout::as_places(&placed, &pairs, Vec2::ZERO);
                let count = changes.len();
                self.set_places(changes, None);
                json!({ "op": "layout", "nodes": count })
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
    fn a_big_batch_without_positions_gets_a_layered_layout() {
        let g = Graph::new();
        // Source, two branches, a join: destinations end up left of their sources.
        run(&g, json!([
            { "op": "create_node", "title": "join" },
            { "op": "create_node", "title": "left" },
            { "op": "create_node", "title": "right" },
            { "op": "create_node", "title": "source" },
            { "op": "link", "from": "$0", "to": "$1" },
            { "op": "link", "from": "$0", "to": "$2" },
            { "op": "link", "from": "$1", "to": "$3" },
            { "op": "link", "from": "$2", "to": "$3" },
        ])).unwrap();
        let p = all_positions(&g);
        assert!(p["source"].x < p["left"].x && p["left"].x < p["join"].x, "{p:?}");
        assert_eq!(p["left"].x, p["right"].x);
        assert_ne!(p["left"].y, p["right"].y);
        assert_eq!(p["source"].x, 0);
        // A later batch of its own lands below this drawing, not on it.
        run(&g, json!([{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b" }, { "op": "create_node", "title": "c" }])).unwrap();
        let q = all_positions(&g);
        assert!(q["a"].y > q["join"].y.max(q["left"].y).max(q["right"].y), "{q:?}");
        // Explicit positions are respected, and small batches place one by one.
        let h = Graph::new();
        run(&h, json!([{ "op": "create_node", "title": "p", "x": 500, "y": 300 }, { "op": "create_node", "title": "q", "x": 0, "y": 0 }, { "op": "create_node", "title": "r", "x": 900, "y": 0 }])).unwrap();
        let r = all_positions(&h);
        assert_eq!((r["p"], r["r"]), (Vec2::new(500, 300), Vec2::new(900, 0)));
    }

    #[test]
    fn a_batch_can_create_nodes_and_link_them_with_dollar_references() {
        let g = Graph::new();
        let results = run(
            &g,
            json!([
                { "op": "create_node", "title": "design" },
                { "op": "create_node", "title": "build", "destinations": ["$0"] },
                { "op": "create_node", "title": "ship", "destinations": ["$1"] },
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
    fn a_source_is_placed_to_the_right_of_its_destination() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b", "destinations": ["$0"] }])).unwrap();
        let (a, b) = (pos_of(&g, "a"), pos_of(&g, "b"));
        assert!(b.x >= a.x + NODE_WIDTH && b.y == a.y, "a at {a:?}, b at {b:?}");
    }

    #[test]
    fn the_old_names_still_work() {
        let g = Graph::new();
        run(&g, json!([
            { "op": "create_node", "title": "a" },
            { "op": "create_node", "title": "b", "depends_on": ["$0"] },
            { "op": "create_node", "title": "c", "dependants": ["$0"] },
            { "op": "link", "from": "$1", "to": "$2", "edge_type": "depends_on" },
        ])).unwrap();
        assert_eq!(g.snapshot().edges.len(), 3);
    }

    #[test]
    fn nodes_created_with_sources_go_before_them() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "late" }])).unwrap();
        run(&g, json!([{ "op": "create_node", "title": "early", "sources": ["#1"] }])).unwrap();
        assert!(pos_of(&g, "early").x < pos_of(&g, "late").x);
        assert_eq!(links(&g), vec!["late>early"]);
    }

    #[test]
    fn creating_many_nodes_never_leaves_them_overlapping() {
        let g = Graph::new();
        let ops: Vec<_> = (0..12).map(|i| json!({ "op": "create_node", "title": format!("n{i}"), "destinations": if i == 0 { vec![] } else { vec!["$0".to_string()] } })).collect();
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
        run(&g, json!([{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b", "destinations": ["$0"] }])).unwrap();
        let before = (links(&g), all_positions(&g));
        // The first two ops are fine; the self-link in the third must undo everything.
        let err = run(
            &g,
            json!([
                { "op": "create_node", "title": "c" },
                { "op": "rename_node", "node": "#1", "title": "renamed" },
                { "op": "link", "from": "#1", "to": "#1" },
            ]),
        )
        .unwrap_err();
        assert_eq!(err.op_index, 2);
        assert!(err.message.contains("itself"), "{}", err.message);
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
        assert!(msg(json!([{ "op": "unlink", "from": "#1", "to": "#1" }])).contains("is not linked"));
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
                { "op": "create_node", "title": "b", "destinations": ["$0"] },
                { "op": "create_node", "title": "c", "destinations": ["$1"] },
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
                { "op": "create_node", "title": "b", "destinations": ["$0"] },
                { "op": "create_node", "title": "c", "destinations": ["$1"] },
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
    fn tidy_places_every_node_clear_of_the_others() {
        let g = Graph::new();
        run(
            &g,
            json!([
                { "op": "create_node", "title": "a", "x": 0, "y": 0 },
                { "op": "create_node", "title": "b", "destinations": ["$0"], "x": 5, "y": 5 },
                { "op": "create_node", "title": "c", "destinations": ["$0"], "x": 9, "y": 9 },
                { "op": "create_node", "title": "d", "destinations": ["$1", "$2"], "x": 11, "y": 11 },
            ]),
        )
        .unwrap();
        let r = run(&g, json!([{ "op": "tidy" }])).unwrap();
        assert_eq!(r[0]["nodes"], 4);
        let p = all_positions(&g);
        let pts: Vec<_> = p.values().copied().collect();
        for (i, a) in pts.iter().enumerate() {
            for b in &pts[i + 1..] {
                assert!((a.x - b.x).abs() >= NODE_WIDTH || (a.y - b.y).abs() >= NODE_HEIGHT, "{a:?} overlaps {b:?}");
            }
        }
        // An empty graph is fine too.
        assert_eq!(run(&Graph::new(), json!([{ "op": "tidy" }])).unwrap()[0]["nodes"], 0);
    }

    #[test]
    fn layout_sets_every_position_and_needs_every_node() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a", "x": 0, "y": 0 }, { "op": "create_node", "title": "b", "destinations": ["$0"], "x": 400, "y": 0 }])).unwrap();
        run(&g, json!([{ "op": "layout", "positions": [{ "node": "#1", "x": 10, "y": 20 }, { "node": "#2", "x": 700, "y": 300 }] }])).unwrap();
        assert_eq!((pos_of(&g, "a"), pos_of(&g, "b")), (Vec2::new(10, 20), Vec2::new(700, 300)));
        let err = run(&g, json!([{ "op": "layout", "positions": [{ "node": "#1", "x": 0, "y": 0 }] }])).unwrap_err();
        assert!(err.message.contains("lacks"), "{}", err.message);
        assert!(run(&g, json!([{ "op": "layout", "positions": [{ "node": "#1", "x": 99999999, "y": 0 }, { "node": "#2", "x": 0, "y": 0 }] }])).is_err());
    }

    #[test]
    fn a_conflicted_link_can_be_removed_and_is_reported() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b", "destinations": ["$0"] }])).unwrap();
        let snap = g.snapshot();
        let (a, b) = (snap.nodes.values().find(|n| n.title == "a").unwrap().id, snap.nodes.values().find(|n| n.title == "b").unwrap().id);
        // A client's link of a to itself arrives and is set aside.
        let _ = b;
        g.create_edge(a, a, waypoint_model::EdgeTypeId::DEFAULT);
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
    fn tags_are_defined_once_and_put_on_nodes_and_links() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b", "destinations": ["$0"] }])).unwrap();
        let err = run(&g, json!([{ "op": "set_tags", "node": "#1", "add": ["todo"] }])).unwrap_err();
        assert!(err.message.contains("no tag"), "{}", err.message);
        run(&g, json!([
            { "op": "upsert_tag", "name": "todo", "glyph": "◻", "color": "#e6b450" },
            { "op": "upsert_tag", "name": "done", "glyph": "✅" },
            { "op": "set_tags", "node": "#1", "add": ["todo", "done"] },
            { "op": "set_tags", "from": "#2", "to": "#1", "add": ["todo"] },
        ])).unwrap();
        let snap = g.snapshot();
        let (a, b) = (snap.nodes.values().find(|n| n.title == "a").unwrap(), snap.nodes.values().find(|n| n.title == "b").unwrap());
        assert_eq!(a.tags.len(), 2);
        assert!(b.tags.is_empty());
        assert_eq!(snap.edge_tags[&(b.id, a.id)].len(), 1);
        let full = crate::api::full(&snap);
        assert_eq!(full["links"][0]["tags"], json!(["todo"]));
        assert_eq!(full["nodes"][0]["tags"].as_array().unwrap().len(), 2);

        // Retyping a link keeps its tags; unlinking and linking again starts clean.
        run(&g, json!([{ "op": "upsert_edge_type", "name": "blocks" }, { "op": "link", "from": "#2", "to": "#1", "edge_type": "blocks" }])).unwrap();
        assert_eq!(g.snapshot().edge_tags[&(b.id, a.id)].len(), 1);
        run(&g, json!([{ "op": "unlink", "from": "#2", "to": "#1" }, { "op": "link", "from": "#2", "to": "#1" }])).unwrap();
        assert!(g.snapshot().edge_tags.get(&(b.id, a.id)).is_none_or(|t| t.is_empty()));

        // Taking a tag off, and removing a tag from the registry.
        run(&g, json!([{ "op": "set_tags", "node": "#1", "remove": ["todo"] }])).unwrap();
        assert_eq!(g.snapshot().nodes[&a.id].tags.len(), 1);
        let r = run(&g, json!([{ "op": "remove_tag", "name": "done" }])).unwrap();
        assert_eq!(r[0]["was_on"], 1);
        assert!(g.snapshot().nodes[&a.id].tags.is_empty(), "a removed tag is gone from what had it");
        assert_eq!(g.snapshot().tags.len(), 1);
    }

    #[test]
    fn tag_ops_check_their_input() {
        let g = Graph::new();
        run(&g, json!([{ "op": "create_node", "title": "a" }])).unwrap();
        let msg = |ops: Value| run(&g, ops).unwrap_err().message;
        assert!(msg(json!([{ "op": "upsert_tag", "name": "x" }])).contains("glyph"));
        assert!(msg(json!([{ "op": "upsert_tag", "name": "x", "glyph": "ab" }])).contains("single glyph"));
        run(&g, json!([{ "op": "upsert_tag", "name": "x", "glyph": "★" }])).unwrap();
        assert!(msg(json!([{ "op": "set_tags", "node": "#1", "add": ["x"], "remove": ["x"] }])).contains("both"));
        assert!(msg(json!([{ "op": "set_tags", "add": ["x"] }])).contains("`node`"));
        assert!(msg(json!([{ "op": "set_tags", "from": "#1", "to": "#1", "add": ["x"] }])).contains("not linked"));
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
        run(&g, json!([{ "op": "create_node", "title": "say \"hi\"" }, { "op": "create_node", "title": "b", "destinations": ["$0"] }])).unwrap();
        let dot = crate::api::dot(&g.snapshot());
        assert!(dot.starts_with("digraph waypoint {"));
        assert!(dot.contains("label=\"#1 say \\\"hi\\\"\""), "{dot}");
        assert_eq!(dot.matches("\" -> \"").count(), 1, "{dot}");
    }

    #[test]
    fn node_types_are_created_styled_and_assigned() {
        let g = Graph::new();
        let r = run(&g, json!([
            { "op": "upsert_node_type", "name": "Risk", "bg": "#7a1f1f", "fg": "#ffffff", "border": "#ff6b6b", "border_width": 3, "shape": "hexagon" },
            { "op": "create_node", "title": "outage", "node_type": "risk" },
            { "op": "create_node", "title": "plain" },
        ])).unwrap();
        assert_eq!(r[0]["created"], true);
        let snap = g.snapshot();
        assert_eq!(snap.node_types.len(), 2);
        let risk = snap.node_types.iter().find(|t| t.name == "risk").unwrap();
        assert_eq!((risk.shape.as_str(), risk.border_width, risk.bg.as_str()), ("hexagon", 3, "#7a1f1f"));
        let by_title = |t: &str| snap.nodes.values().find(|n| n.title == t).unwrap().clone();
        assert_eq!(snap.node_type(by_title("outage").type_id).name, "risk");
        assert_eq!(snap.node_type(by_title("plain").type_id).name, "default");

        // Changing one field leaves the others; removing the type puts its nodes back to default.
        run(&g, json!([{ "op": "upsert_node_type", "name": "risk", "border_width": 99 }])).unwrap();
        let snap = g.snapshot();
        let risk = snap.node_types.iter().find(|t| t.name == "risk").unwrap();
        assert_eq!((risk.border_width, risk.shape.as_str()), (10, "hexagon"), "width is clamped, shape kept");
        run(&g, json!([{ "op": "remove_node_type", "name": "risk" }])).unwrap();
        let snap = g.snapshot();
        assert_eq!(snap.node_types.len(), 1);
        assert_eq!(snap.node_type(by_title("outage").type_id).name, "default");
        assert!(run(&g, json!([{ "op": "remove_node_type", "name": "default" }])).is_err());
    }

    #[test]
    fn colours_must_come_in_pairs_and_contrast() {
        let g = Graph::new();
        let err = |op: Value| run(&g, json!([op])).unwrap_err().message;
        assert!(err(json!({ "op": "upsert_node_type", "name": "a", "bg": "#000000" })).contains("together"));
        assert!(err(json!({ "op": "upsert_node_type", "name": "a", "bg": "#777777", "fg": "#808080" })).contains("contrast"));
        // Edge text and line must read on the document background (dark by default).
        assert!(err(json!({ "op": "upsert_edge_type", "name": "e", "text_color": "#222222" })).contains("text_color"));
        assert!(err(json!({ "op": "upsert_edge_type", "name": "e", "color": "#1a1c20" })).contains("color"));
        assert!(g.snapshot().node_types.len() == 1 && g.snapshot().edge_types.len() == 1, "nothing was kept");
    }

    #[test]
    fn edges_take_a_type_and_can_be_retyped() {
        let g = Graph::new();
        run(&g, json!([
            { "op": "upsert_edge_type", "name": "blocks", "color": "#ffb454", "width": 0, "start": "dot", "end": "bar", "line_style": "dashed" },
            { "op": "create_node", "title": "a" },
            { "op": "create_node", "title": "b" },
            { "op": "link", "from": "$1", "to": "$2", "edge_type": "blocks" },
        ])).unwrap();
        let snap = g.snapshot();
        let e = snap.edge_type(snap.edges[0].type_id);
        assert_eq!((e.name.as_str(), e.width, e.start.as_str(), e.end.as_str()), ("blocks", 1, "dot", "bar"));
        let r = run(&g, json!([{ "op": "link", "from": "#1", "to": "#2", "edge_type": "link" }])).unwrap();
        assert_eq!(r[0]["already_linked"], true);
        assert_eq!(g.snapshot().edge_type(g.snapshot().edges[0].type_id).name, "link");
        assert!(run(&g, json!([{ "op": "link", "from": "#1", "to": "#2", "edge_type": "nope" }])).unwrap_err().message.contains("no edge type"));
    }

    #[test]
    fn a_partly_written_type_keeps_the_built_in_label() {
        let g = Graph::new();
        g.apply_writes(&[Write::UpsertType {
            kind: Kind::Edge,
            id: style::DEFAULT_EDGE_TYPE,
            fields: vec![("color".into(), json!("#ffffff"))],
        }]);
        assert_eq!(g.snapshot().edge_types[0].label, "link");
    }

    #[test]
    fn an_entry_written_with_the_old_rounded_shape_reads_as_rect_with_round_corners() {
        let g = Graph::new();
        let id = uuid::Uuid::now_v7();
        g.apply_writes(&[Write::UpsertType {
            kind: Kind::Node,
            id,
            fields: vec![("name".into(), json!("old")), ("shape".into(), json!("rounded")), ("border_style".into(), json!("dashed"))],
        }]);
        let snap = g.snapshot();
        let t = snap.node_types.iter().find(|t| t.id == id).unwrap();
        assert_eq!((t.shape.as_str(), t.corners.as_str()), ("rect", "round"));
    }

    #[test]
    fn an_edge_type_may_have_no_label() {
        let g = Graph::new();
        run(&g, json!([{ "op": "upsert_edge_type", "name": "quiet", "label": "" }])).unwrap();
        let snap = g.snapshot();
        assert_eq!(snap.edge_types.iter().find(|t| t.name == "quiet").unwrap().label, "");
        // And the default type can be silenced without losing its other settings.
        run(&g, json!([{ "op": "upsert_edge_type", "name": "link", "label": "" }])).unwrap();
        let snap = g.snapshot();
        let d = snap.edge_types.iter().find(|t| t.name == "link").unwrap();
        assert_eq!((d.label.as_str(), d.width), ("", 2));
    }
}
