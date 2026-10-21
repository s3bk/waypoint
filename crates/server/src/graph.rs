//! A graph as a Yjs document, following DATA_MODEL.md "Document shape".
//! `nodes` is a `Y.Map<NodeId, Y.Map>` and `edges` a `Y.Map<EdgeKey, Y.Map>`;
//! each field is its own key so concurrent edits to different fields both
//! survive. `place` is one key holding the whole (anchor, offset) pair, so it
//! is last-writer-wins as a unit.
//!
//! An edge is identified by the ordered pair of its ends: `"{from}>{to}"`.
//! Older documents used `"{from}>{to}:{type}"`; readers go by the `from` and
//! `to` fields, not the key, so both spellings are understood.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::Mutex,
};

use uuid::Uuid;
use waypoint_model::{
    EdgeTypeId, NodeId, Place, Vec2,
    style::{self, EdgeType, Kind, NodeType, Tag},
};
use yrs::{
    Any, Array, ArrayRef, Doc, GetString, Map, MapPrelim, MapRef, Origin, Out, ReadTxn, StateVector, Text, TextPrelim,
    Transact, Update,
    types::ToJson,
    undo::{Options as UndoOptions, UndoManager},
};

pub struct Graph {
    doc: Doc,
    nodes: MapRef,
    edges: MapRef,
    /// Document-level settings; today only `title`, which only the owner route writes.
    meta: MapRef,
    /// The type registries: `Y.Map<type id, Y.Map>`, one field per key. Only
    /// entries that differ from the built-in defaults need to exist.
    node_types: MapRef,
    tags: MapRef,
    edge_types: MapRef,
    /// Who changed what: one JSON string per batch, appended in the same
    /// transaction as the batch. Not tracked by undo, so undoing leaves a trace.
    log: ArrayRef,
    /// One undo history per actor, created before the actor's first edit. Only
    /// edits made with that actor as origin are in it, so server repairs (no
    /// origin) and other actors' edits are never undone by accident. In memory:
    /// a restart empties the stacks.
    undo: Mutex<HashMap<String, UndoManager<()>>>,
}

/// A live node, as read from the document.
#[derive(Clone, Debug, PartialEq)]
pub struct NodeRec {
    pub id: NodeId,
    /// Short human-friendly key, assigned by the server. `None` until then.
    pub key: Option<u64>,
    pub title: String,
    pub place: Place,
    /// The node's shared note (empty until someone writes one).
    pub note: String,
    /// The node's type; `None` means the default node type.
    pub type_id: Option<Uuid>,
    /// Ids of the tags on the node (only tags that exist).
    pub tags: BTreeSet<Uuid>,
}

/// A live edge: `from` links to `to`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EdgeRec {
    pub from: NodeId,
    pub to: NodeId,
    pub type_id: EdgeTypeId,
}

/// A consistent read of the live graph, for planning edits without holding a
/// document transaction open.
#[derive(Clone, Debug, Default)]
pub struct Snapshot {
    pub nodes: BTreeMap<NodeId, NodeRec>,
    /// One per ordered pair, both ends live.
    pub edges: Vec<EdgeRec>,
    /// Edges the server set aside because they closed a cycle. They
    /// are not part of `edges`, so planning never sees them.
    pub conflicted: Vec<EdgeRec>,
    /// Edge entries still marked live whose end is a deleted or unknown node, e.g. a link one
    /// client added to a node another had just deleted. Repair removes them.
    pub dangling: Vec<(NodeId, NodeId)>,
    /// The graph's name, set by the owner; empty until then.
    pub title: String,
    /// The registries as they stand: built-in defaults overlaid with the document's entries, removed ones left out.
    pub node_types: Vec<NodeType>,
    pub edge_types: Vec<EdgeType>,
    pub tags: Vec<Tag>,
    /// Tags on each live link, by ordered pair (only tags that exist).
    pub edge_tags: BTreeMap<(NodeId, NodeId), BTreeSet<Uuid>>,
    /// The canvas colour, set by the owner; edge colours and text are kept readable on it.
    pub background: String,
}

impl Snapshot {
    /// The entry for a node's type; a missing or removed type shows as the default.
    pub fn node_type(&self, id: Option<Uuid>) -> &NodeType {
        let default = || self.node_types.iter().find(|t| t.id == style::DEFAULT_NODE_TYPE).expect("default node type");
        id.and_then(|id| self.node_types.iter().find(|t| t.id == id)).unwrap_or_else(default)
    }

    pub fn edge_type(&self, id: EdgeTypeId) -> &EdgeType {
        let default = || self.edge_types.iter().find(|t| t.id == style::DEFAULT_EDGE_TYPE).expect("default edge type");
        self.edge_types.iter().find(|t| t.id == id.0).unwrap_or_else(default)
    }

    pub fn places(&self) -> BTreeMap<NodeId, Place> {
        self.nodes.iter().map(|(&id, n)| (id, n.place)).collect()
    }
}

/// What a tag is put on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagTarget {
    Node(NodeId),
    Link(NodeId, NodeId),
}

/// The key under which an entry records that it has a tag.
fn tag_key(tag: Uuid) -> String {
    format!("tag:{tag}")
}

/// The tags an entry's fields say it has.
fn tags_of(f: &HashMap<String, Any>) -> BTreeSet<Uuid> {
    f.iter().filter(|(k, v)| matches!(v, Any::Bool(true)) && k.starts_with("tag:")).filter_map(|(k, _)| k[4..].parse().ok()).collect()
}

/// One change to the document. A list of these is applied as a single
/// transaction by [`Graph::apply_writes`].
#[derive(Clone, Debug, PartialEq)]
pub enum Write {
    CreateNode { id: NodeId, title: String, place: Place },
    SetPlace { id: NodeId, place: Place },
    SetTitle { id: NodeId, title: String },
    /// Create the edge, or bring it back if it was removed.
    Link { from: NodeId, to: NodeId, type_id: EdgeTypeId },
    /// Remove the edge between this ordered pair.
    Unlink { from: NodeId, to: NodeId },
    /// Soft-delete the node and every edge touching it.
    DeleteNode { id: NodeId },
    /// Set the graph's own title.
    SetGraphTitle { title: String },
    /// Flag the edge between this ordered pair as closing a cycle. Linking the
    /// pair again writes a fresh entry, which clears the flag.
    MarkConflicted { from: NodeId, to: NodeId },
    /// Give a node a type (`None`: the default).
    SetNodeType { id: NodeId, type_id: Option<Uuid> },
    /// Create or change a registry entry by writing these fields (the planner
    /// passes only the ones that changed, so edits to different fields merge).
    UpsertType { kind: Kind, id: Uuid, fields: Vec<(String, serde_json::Value)> },
    /// Set the canvas colour.
    SetBackground { color: String },
    /// Mark a registry entry removed.
    RemoveType { kind: Kind, id: Uuid },
    /// Put a tag on, or take it off, a node or a link.
    SetTag { target: TagTarget, tag: Uuid, on: bool },
    /// Change the node's note by editing only the part that differs.
    SetNote { id: NodeId, body: String },
    /// Add text at the end of the node's note.
    AppendNote { id: NodeId, text: String },
}

fn edge_key(from: NodeId, to: NodeId) -> String {
    format!("{}>{}", from.0, to.0)
}

fn any_str(a: Option<&Any>) -> Option<&str> {
    match a {
        Some(Any::String(s)) => Some(s),
        _ => None,
    }
}

fn any_key(a: Option<&Any>) -> Option<u64> {
    match a {
        Some(Any::Number(n)) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u64),
        Some(Any::BigInt(n)) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

fn any_bool(a: Option<&Any>) -> bool {
    matches!(a, Some(Any::Bool(true)))
}

/// How to turn `old` into `new` with one edit: the byte offset to start at, how
/// many bytes to remove there, and what to insert. Only the middle that differs
/// is touched (the document counts text in bytes, see `Doc::new`).
fn text_diff<'a>(old: &str, new: &'a str) -> (usize, usize, &'a str) {
    let prefix = old.chars().zip(new.chars()).take_while(|(a, b)| a == b).map(|(a, _)| a.len_utf8()).sum::<usize>();
    let (o, n) = (&old[prefix..], &new[prefix..]);
    let suffix =
        o.chars().rev().zip(n.chars().rev()).take_while(|(a, b)| a == b).map(|(a, _)| a.len_utf8()).sum::<usize>();
    (prefix, o.len() - suffix, &n[..n.len() - suffix])
}

fn any_u32(a: Option<&Any>) -> Option<u32> {
    match a {
        Some(Any::Number(n)) if *n >= 0.0 && n.fract() == 0.0 => Some(*n as u32),
        _ => None,
    }
}

/// The fields of `new` that differ from `old` (all of them if there is no `old`), for [`Write::UpsertType`].
pub fn node_diff(old: Option<&NodeType>, new: &NodeType) -> Vec<(String, serde_json::Value)> {
    use serde_json::{Value, json};
    let all: [(&str, Value, Option<Value>); 7] = [
        ("name", json!(new.name), old.map(|o| json!(o.name))),
        ("shape", json!(new.shape), old.map(|o| json!(o.shape))),
        ("bg", json!(new.bg), old.map(|o| json!(o.bg))),
        ("fg", json!(new.fg), old.map(|o| json!(o.fg))),
        ("border", json!(new.border), old.map(|o| json!(o.border))),
        ("border_width", json!(new.border_width), old.map(|o| json!(o.border_width))),
        ("corners", json!(new.corners), old.map(|o| json!(o.corners))),
    ];
    all.into_iter().filter(|(_, n, o)| o.as_ref() != Some(n)).map(|(k, n, _)| (k.to_owned(), n)).collect()
}

pub fn tag_diff(old: Option<&Tag>, new: &Tag) -> Vec<(String, serde_json::Value)> {
    use serde_json::{Value, json};
    let all: [(&str, Value, Option<Value>); 3] = [
        ("name", json!(new.name), old.map(|o| json!(o.name))),
        ("glyph", json!(new.glyph), old.map(|o| json!(o.glyph))),
        ("color", json!(new.color), old.map(|o| json!(o.color))),
    ];
    all.into_iter().filter(|(_, n, o)| o.as_ref() != Some(n)).map(|(k, n, _)| (k.to_owned(), n)).collect()
}

pub fn edge_diff(old: Option<&EdgeType>, new: &EdgeType) -> Vec<(String, serde_json::Value)> {
    use serde_json::{Value, json};
    let all: [(&str, Value, Option<Value>); 8] = [
        ("name", json!(new.name), old.map(|o| json!(o.name))),
        ("label", json!(new.label), old.map(|o| json!(o.label))),
        ("color", json!(new.color), old.map(|o| json!(o.color))),
        ("width", json!(new.width), old.map(|o| json!(o.width))),
        ("line_style", json!(new.line_style), old.map(|o| json!(o.line_style))),
        ("start", json!(new.start), old.map(|o| json!(o.start))),
        ("end", json!(new.end), old.map(|o| json!(o.end))),
        ("text_color", json!(new.text_color), old.map(|o| json!(o.text_color))),
    ];
    all.into_iter().filter(|(_, n, o)| o.as_ref() != Some(n)).map(|(k, n, _)| (k.to_owned(), n)).collect()
}

fn parse_node(key: &str) -> Option<NodeId> {
    key.parse::<Uuid>().ok().map(NodeId)
}

impl Graph {
    pub fn new() -> Self {
        let doc = Doc::new();
        let nodes = doc.get_or_insert_map("nodes");
        let edges = doc.get_or_insert_map("edges");
        let meta = doc.get_or_insert_map("meta");
        let node_types = doc.get_or_insert_map("node_types");
        let edge_types = doc.get_or_insert_map("edge_types");
        let tags = doc.get_or_insert_map("tags");
        let log = doc.get_or_insert_array("log");
        Self { doc, nodes, edges, meta, node_types, edge_types, tags, log, undo: Mutex::new(HashMap::new()) }
    }

    /// Insert a node. Used for seeding and tests; edits from the API go through
    /// [`Graph::apply_writes`].
    pub fn create_node(&self, id: NodeId, title: &str, place: Place) {
        self.apply_writes(&[Write::CreateNode { id, title: title.to_owned(), place }]);
    }

    pub fn create_edge(&self, from: NodeId, to: NodeId, type_id: EdgeTypeId) {
        self.apply_writes(&[Write::Link { from, to, type_id }]);
    }

    /// Apply a list of writes as one transaction and return the encoded update
    /// (for relaying to connected clients).
    pub fn apply_writes(&self, writes: &[Write]) -> Vec<u8> {
        self.write_in(self.doc.transact_mut(), writes, None)
    }

    /// Like [`Graph::apply_writes`], but on behalf of `actor`: the edit goes
    /// into that actor's undo history and `entry` (JSON) into the change log.
    pub fn apply_writes_as(&self, actor: &str, entry: String, writes: &[Write]) -> Vec<u8> {
        self.ensure_undo(actor);
        self.write_in(self.doc.transact_mut_with(actor), writes, Some(entry))
    }

    fn ensure_undo(&self, actor: &str) {
        let mut all = self.undo.lock().expect("undo lock");
        if all.contains_key(actor) {
            return;
        }
        let options = UndoOptions {
            capture_timeout_millis: 0, // one history item per batch
            tracked_origins: HashSet::from([Origin::from(actor)]),
            ..UndoOptions::default()
        };
        let mut manager = UndoManager::with_scope_and_options(&self.doc, &self.nodes, options);
        manager.expand_scope(&self.edges);
        manager.expand_scope(&self.node_types);
        manager.expand_scope(&self.edge_types);
        manager.expand_scope(&self.tags);
        all.insert(actor.to_owned(), manager);
    }

    /// Undo (or redo) the actor's latest batch. Returns the update to relay, or
    /// `None` if there was nothing to undo.
    pub fn undo_as(&self, actor: &str, redo: bool, entry: String) -> Option<Vec<u8>> {
        let before = self.doc.transact().state_vector();
        {
            let mut all = self.undo.lock().expect("undo lock");
            let manager = all.get_mut(actor)?;
            let done = if redo { manager.redo_blocking() } else { manager.undo_blocking() };
            if !done {
                return None;
            }
        }
        let mut txn = self.doc.transact_mut();
        self.log.push_back(&mut txn, entry);
        drop(txn);
        Some(self.doc.transact().encode_diff_v1(&before))
    }

    /// The change log, oldest first.
    pub fn log(&self) -> Vec<serde_json::Value> {
        let txn = self.doc.transact();
        self.log
            .iter(&txn)
            .filter_map(|v| match v {
                Out::Any(Any::String(s)) => serde_json::from_str(&s).ok(),
                _ => None,
            })
            .collect()
    }

    fn write_in(&self, mut txn: yrs::TransactionMut<'_>, writes: &[Write], entry: Option<String>) -> Vec<u8> {
        if let Some(entry) = entry {
            self.log.push_back(&mut txn, entry);
        }
        for w in writes {
            match w {
                Write::CreateNode { id, title, place } => {
                    let node = self.nodes.insert(
                        &mut txn,
                        id.0.to_string(),
                        MapPrelim::from([
                            ("title".to_string(), Any::from(title.as_str())),
                            ("place".to_string(), Any::from(serde_json::to_string(place).expect("place serializes"))),
                            ("deleted".to_string(), Any::from(false)),
                        ]),
                    );
                    // Created here, once, so two people typing never each create their own text.
                    node.insert(&mut txn, "note", TextPrelim::new(""));
                }
                Write::SetPlace { id, place } => {
                    if let Some(Out::YMap(m)) = self.nodes.get(&txn, &id.0.to_string()) {
                        m.insert(&mut txn, "place", Any::from(serde_json::to_string(place).expect("place serializes")));
                    }
                }
                Write::SetTitle { id, title } => {
                    if let Some(Out::YMap(m)) = self.nodes.get(&txn, &id.0.to_string()) {
                        m.insert(&mut txn, "title", Any::from(title.as_str()));
                    }
                }
                Write::Link { from, to, type_id } => {
                    // Changing the type of a live link keeps its tags.
                    let kept = match self.edges.get(&txn, &edge_key(*from, *to)) {
                        Some(Out::YMap(m)) => match m.to_json(&txn) {
                            Any::Map(f) if !any_bool(f.get("deleted")) => tags_of(&f),
                            _ => BTreeSet::new(),
                        },
                        _ => BTreeSet::new(),
                    };
                    let entry = self.edges.insert(
                        &mut txn,
                        edge_key(*from, *to),
                        MapPrelim::from([
                            ("from".to_string(), Any::from(from.0.to_string())),
                            ("to".to_string(), Any::from(to.0.to_string())),
                            ("type".to_string(), Any::from(type_id.0.to_string())),
                            ("deleted".to_string(), Any::from(false)),
                        ]),
                    );
                    for tag in kept {
                        entry.insert(&mut txn, tag_key(tag), Any::from(true));
                    }
                }
                Write::Unlink { from, to } => self.delete_edges(&mut txn, |f, t| f == *from && t == *to),
                Write::SetGraphTitle { title } => {
                    self.meta.insert(&mut txn, "title", Any::from(title.as_str()));
                }
                Write::MarkConflicted { from, to } => {
                    self.update_edges(&mut txn, |f, t| f == *from && t == *to, "conflicted");
                }
                Write::SetNodeType { id, type_id } => {
                    if let Some(Out::YMap(m)) = self.nodes.get(&txn, &id.0.to_string()) {
                        let value = type_id.map(|t| t.to_string()).unwrap_or_default();
                        m.insert(&mut txn, "type", Any::from(value));
                    }
                }
                Write::UpsertType { kind, id, fields } => {
                    let registry = self.registry(*kind);
                    let key = id.to_string();
                    let entry = match registry.get(&txn, &key) {
                        Some(Out::YMap(m)) => m,
                        _ => registry.insert(&mut txn, key, MapPrelim::from([("id".to_string(), Any::from(id.to_string()))])),
                    };
                    for (field, value) in fields {
                        let value = match value {
                            serde_json::Value::String(s) => Any::from(s.as_str()),
                            serde_json::Value::Number(n) => Any::Number(n.as_f64().unwrap_or(0.0)),
                            _ => continue,
                        };
                        entry.insert(&mut txn, field.as_str(), value);
                    }
                    entry.insert(&mut txn, "removed", Any::from(false));
                }
                Write::SetBackground { color } => {
                    self.meta.insert(&mut txn, "background", Any::from(color.as_str()));
                }
                Write::RemoveType { kind, id } => {
                    let registry = self.registry(*kind);
                    let key = id.to_string();
                    match registry.get(&txn, &key) {
                        Some(Out::YMap(m)) => {
                            m.insert(&mut txn, "removed", Any::from(true));
                        }
                        _ => {
                            registry.insert(
                                &mut txn,
                                key,
                                MapPrelim::from([
                                    ("id".to_string(), Any::from(id.to_string())),
                                    ("removed".to_string(), Any::from(true)),
                                ]),
                            );
                        }
                    }
                }
                Write::DeleteNode { id } => {
                    if let Some(Out::YMap(m)) = self.nodes.get(&txn, &id.0.to_string()) {
                        m.insert(&mut txn, "deleted", Any::from(true));
                    }
                    self.delete_edges(&mut txn, |f, t| f == *id || t == *id);
                }
                Write::SetTag { target, tag, on } => match target {
                    TagTarget::Node(id) => {
                        if let Some(Out::YMap(m)) = self.nodes.get(&txn, &id.0.to_string()) {
                            m.insert(&mut txn, tag_key(*tag), Any::from(*on));
                        }
                    }
                    TagTarget::Link(from, to) => self.update_edges_with(&mut txn, |f, t| f == *from && t == *to, &tag_key(*tag), *on),
                },
                Write::SetNote { id, body } => {
                    if let Some(text) = self.note_text(&mut txn, *id) {
                        let old = text.get_string(&txn);
                        let (start, cut, insert) = text_diff(&old, body);
                        if cut > 0 {
                            text.remove_range(&mut txn, start as u32, cut as u32);
                        }
                        if !insert.is_empty() {
                            text.insert(&mut txn, start as u32, insert);
                        }
                    }
                }
                Write::AppendNote { id, text: added } => {
                    if let Some(text) = self.note_text(&mut txn, *id) {
                        let end = text.len(&txn);
                        let add = if end > 0 { format!("\n\n{added}") } else { added.clone() };
                        text.insert(&mut txn, end, &add);
                    }
                }
            }
        }
        txn.encode_update_v1()
    }

    fn registry(&self, kind: Kind) -> &MapRef {
        match kind {
            Kind::Node => &self.node_types,
            Kind::Edge => &self.edge_types,
            Kind::Tag => &self.tags,
        }
    }

    /// The raw entries of a registry that are not removed. A built-in entry can
    /// be overridden but not removed.
    fn registry_entries(&self, txn: &impl ReadTxn, kind: Kind) -> Vec<(Uuid, HashMap<String, Any>)> {
        let builtin = match kind {
            Kind::Node => style::DEFAULT_NODE_TYPE,
            Kind::Edge => style::DEFAULT_EDGE_TYPE,
            Kind::Tag => Uuid::nil(),
        };
        let Any::Map(entries) = self.registry(kind).to_json(txn) else { return Vec::new() };
        entries
            .iter()
            .filter_map(|(key, entry)| {
                let (Ok(id), Any::Map(f)) = (key.parse::<Uuid>(), entry) else { return None };
                (!any_bool(f.get("removed")) || id == builtin).then(|| (id, f.as_ref().clone()))
            })
            .collect()
    }

    /// The node types as they stand: the built-in default overlaid with the document's
    /// entries. A field that is missing or not allowed falls back to the default, so a
    /// hand-made or half-merged entry still draws. When two entries share a name the one
    /// with the earlier id keeps it and the other is hidden.
    fn read_node_types(&self, txn: &impl ReadTxn) -> Vec<NodeType> {
        let builtin = style::builtin_node();
        let mut defs: BTreeMap<Uuid, NodeType> = BTreeMap::from([(builtin.id, builtin.clone())]);
        for (id, f) in self.registry_entries(txn, Kind::Node) {
            let base = defs.get(&id).cloned().unwrap_or_else(|| NodeType { id, name: id.to_string(), ..builtin.clone() });
            let text = |k: &str, fallback: &str| any_str(f.get(k)).unwrap_or(fallback).to_owned();
            let color = |k: &str, fallback: &str| any_str(f.get(k)).and_then(|c| style::normalize_color(c).ok()).unwrap_or_else(|| fallback.to_owned());
            let name = style::normalize_name(&text("name", &base.name)).unwrap_or_else(|_| base.name.clone());
            let mut def = NodeType {
                id,
                name,
                shape: text("shape", &base.shape),
                bg: color("bg", &base.bg),
                fg: color("fg", &base.fg),
                border: color("border", &base.border),
                border_width: any_u32(f.get("border_width")).map(style::border_width).unwrap_or(base.border_width),
                corners: text("corners", &base.corners),
            };
            if def.shape == "rounded" {
                // Written before corners existed.
                def.shape = "rect".into();
                if f.get("corners").is_none() {
                    def.corners = "round".into();
                }
            }
            if !style::NODE_SHAPES.contains(&def.shape.as_str()) {
                def.shape = base.shape.clone();
            }
            if !style::CORNERS.contains(&def.corners.as_str()) {
                def.corners = base.corners.clone();
            }
            if style::check_contrast("", &def.bg, &def.fg).is_err() {
                (def.bg, def.fg) = (builtin.bg.clone(), builtin.fg.clone());
            }
            defs.insert(id, def);
        }
        let mut seen = BTreeSet::new();
        defs.into_values().filter(|d| seen.insert(d.name.clone())).collect()
    }

    /// The tags: every live registry entry that has a usable name and glyph (a half-written one is left out).
    /// When two share a name the one with the earlier id keeps it.
    fn read_tags(&self, txn: &impl ReadTxn) -> Vec<Tag> {
        let mut found = BTreeMap::new();
        for (id, f) in self.registry_entries(txn, Kind::Tag) {
            let Some(name) = any_str(f.get("name")).and_then(|n| style::normalize_name(n).ok()) else { continue };
            let Some(glyph) = any_str(f.get("glyph")).map(str::trim).filter(|g| style::is_one_glyph(g)) else { continue };
            let color = any_str(f.get("color")).and_then(|c| style::normalize_color(c).ok()).unwrap_or_else(|| style::builtin_tag_color().into());
            found.insert(id, Tag { id, name, glyph: glyph.to_owned(), color });
        }
        let mut seen = BTreeSet::new();
        found.into_values().filter(|t| seen.insert(t.name.clone())).collect()
    }

    /// Like [`Graph::read_node_types`]; the line and text colours must also read on `background`,
    /// and fall back to black or white if the background was changed under them.
    fn read_edge_types(&self, txn: &impl ReadTxn, background: &str) -> Vec<EdgeType> {
        let builtin = style::builtin_edge();
        let mut defs: BTreeMap<Uuid, EdgeType> = BTreeMap::from([(builtin.id, builtin.clone())]);
        for (id, f) in self.registry_entries(txn, Kind::Edge) {
            let known = defs.contains_key(&id);
            let base = defs.get(&id).cloned().unwrap_or_else(|| EdgeType { id, name: id.to_string(), ..builtin.clone() });
            let text = |k: &str, fallback: &str| any_str(f.get(k)).unwrap_or(fallback).to_owned();
            let color = |k: &str, fallback: &str| any_str(f.get(k)).and_then(|c| style::normalize_color(c).ok()).unwrap_or_else(|| fallback.to_owned());
            let name = style::normalize_name(&text("name", &base.name)).unwrap_or_else(|_| base.name.clone());
            let pick = |k: &str, fallback: &str, allowed: &[&str]| {
                let v = text(k, fallback);
                if allowed.contains(&v.as_str()) { v } else { fallback.to_owned() }
            };
            let def = EdgeType {
                id,
                // An empty label is a label (nothing is drawn); only a missing one falls back.
                label: any_str(f.get("label")).unwrap_or(if known { &base.label } else { &name }).to_owned(),
                name,
                color: color("color", &base.color),
                width: any_u32(f.get("width")).map(style::edge_width).unwrap_or(base.width),
                line_style: pick("line_style", &base.line_style, style::LINE_STYLES),
                start: pick("start", &base.start, style::ARROWS),
                end: pick("end", &base.end, style::ARROWS),
                text_color: color("text_color", &base.text_color),
            };
            defs.insert(id, def);
        }
        let readable = style::readable_on(background);
        for d in defs.values_mut() {
            if style::check_line_contrast("", background, &d.color).is_err() {
                d.color = readable.into();
            }
            if style::check_contrast("", background, &d.text_color).is_err() {
                d.text_color = readable.into();
            }
        }
        let mut seen = BTreeSet::new();
        defs.into_values().filter(|d| seen.insert(d.name.clone())).collect()
    }

    /// The node's note text, created if an older node has none.
    fn note_text(&self, txn: &mut yrs::TransactionMut, id: NodeId) -> Option<yrs::TextRef> {
        let Some(Out::YMap(m)) = self.nodes.get(txn, &id.0.to_string()) else { return None };
        match m.get(txn, "note") {
            Some(Out::YText(t)) => Some(t),
            _ => Some(m.insert(txn, "note", TextPrelim::new(""))),
        }
    }

    /// Mark every edge entry whose ends satisfy `matches` as deleted. Goes by
    /// the `from`/`to` fields, so entries under an older key spelling count too.
    fn delete_edges(&self, txn: &mut yrs::TransactionMut, matches: impl Fn(NodeId, NodeId) -> bool) {
        self.update_edges(txn, matches, "deleted");
    }

    /// Set the boolean `flag` on every edge entry whose ends satisfy `matches`.
    fn update_edges(&self, txn: &mut yrs::TransactionMut, matches: impl Fn(NodeId, NodeId) -> bool, flag: &str) {
        self.update_edges_with(txn, matches, flag, true);
    }

    fn update_edges_with(&self, txn: &mut yrs::TransactionMut, matches: impl Fn(NodeId, NodeId) -> bool, flag: &str, value: bool) {
        let mut hit = Vec::new();
        for (key, value) in self.edges.iter(txn) {
            let Out::YMap(m) = value else { continue };
            let (Some(Any::String(f)), Some(Any::String(t))) = (read_any(&m, txn, "from"), read_any(&m, txn, "to")) else {
                continue;
            };
            if let (Some(f), Some(t)) = (parse_node(&f), parse_node(&t)) {
                if matches(f, t) {
                    hit.push((key.to_string(), m));
                }
            }
        }
        for (_, m) in hit {
            m.insert(txn, flag, Any::from(value));
        }
    }

    /// Read the live graph.
    pub fn snapshot(&self) -> Snapshot {
        let txn = self.doc.transact();
        let mut snap = Snapshot::default();
        snap.title = match self.meta.get(&txn, "title") {
            Some(Out::Any(Any::String(t))) => t.to_string(),
            _ => String::new(),
        };
        snap.background = match self.meta.get(&txn, "background") {
            Some(Out::Any(Any::String(c))) => style::normalize_color(&c).unwrap_or_else(|_| style::DEFAULT_BACKGROUND.into()),
            _ => style::DEFAULT_BACKGROUND.into(),
        };
        snap.node_types = self.read_node_types(&txn);
        snap.edge_types = self.read_edge_types(&txn, &snap.background);
        snap.tags = self.read_tags(&txn);
        let known: BTreeSet<Uuid> = snap.tags.iter().map(|t| t.id).collect();
        if let Any::Map(nodes) = self.nodes.to_json(&txn) {
            for (key, node) in nodes.iter() {
                let (Some(id), Any::Map(f)) = (parse_node(key), node) else { continue };
                if any_bool(f.get("deleted")) {
                    continue;
                }
                let Some(place) = any_str(f.get("place")).and_then(|p| serde_json::from_str(p).ok()) else { continue };
                snap.nodes.insert(
                    id,
                    NodeRec {
                        id,
                        key: any_key(f.get("key")),
                        title: any_str(f.get("title")).unwrap_or("").to_owned(),
                        place,
                        note: any_str(f.get("note")).unwrap_or("").to_owned(),
                        type_id: any_str(f.get("type")).and_then(|t| t.parse::<Uuid>().ok()),
                        tags: tags_of(f).intersection(&known).copied().collect(),
                    },
                );
            }
        }
        // An edge pair is live if any entry for it is live (entries may exist
        // under both key spellings); the canonical entry's type wins. It is
        // conflicted only if every live entry is flagged.
        let mut live: BTreeMap<(NodeId, NodeId), EdgeRec> = BTreeMap::new();
        let mut flagged: BTreeMap<(NodeId, NodeId), EdgeRec> = BTreeMap::new();
        if let Any::Map(edges) = self.edges.to_json(&txn) {
            let mut entries: Vec<_> = edges.iter().collect();
            entries.sort_by_key(|(k, _)| k.contains(':')); // canonical keys first
            for (_, edge) in entries {
                let Any::Map(f) = edge else { continue };
                if any_bool(f.get("deleted")) {
                    continue;
                }
                let (Some(from), Some(to)) = (
                    any_str(f.get("from")).and_then(parse_node),
                    any_str(f.get("to")).and_then(parse_node),
                ) else {
                    continue;
                };
                if !snap.nodes.contains_key(&from) || !snap.nodes.contains_key(&to) {
                    if !snap.dangling.contains(&(from, to)) {
                        snap.dangling.push((from, to));
                    }
                    continue;
                }
                let type_id = any_str(f.get("type"))
                    .and_then(|t| t.parse::<Uuid>().ok())
                    .map(EdgeTypeId)
                    .unwrap_or(EdgeTypeId::DEFAULT);
                let rec = EdgeRec { from, to, type_id };
                snap.edge_tags.entry((from, to)).or_default().extend(tags_of(f).intersection(&known));
                // Only a link to itself is set aside now; older graphs flagged links that closed a cycle, which are fine.
                if any_bool(f.get("conflicted")) && from == to {
                    flagged.entry((from, to)).or_insert(rec);
                } else {
                    live.entry((from, to)).or_insert(rec);
                }
            }
        }
        flagged.retain(|pair, _| !live.contains_key(pair));
        snap.edges = live.into_values().collect();
        snap.conflicted = flagged.into_values().collect();
        snap
    }

    /// Places of all live nodes, ready for `waypoint_layout::derive_positions`.
    pub fn places(&self) -> BTreeMap<NodeId, Place> {
        self.snapshot().places()
    }

    /// Give every node without a key the next number. Returns the update to
    /// relay, or `None` if nothing needed a key.
    ///
    /// Keys are short, human-friendly names (`#12`) that people and agents can
    /// type. They are assigned only here, so two clients adding nodes at once
    /// can never get the same number; nodes are numbered in id order, and ids are
    /// time-ordered UUIDs, so that is creation order. A key is never reused or
    /// changed: deleted nodes keep theirs (they are only soft-deleted), so the
    /// next number is one past the highest key ever seen.
    pub fn assign_keys(&self) -> Option<Vec<u8>> {
        let mut txn = self.doc.transact_mut();
        let Any::Map(nodes) = self.nodes.to_json(&txn) else { return None };
        let mut highest = 0;
        let mut missing = BTreeSet::new();
        let mut unnoted = Vec::new();
        for (key, node) in nodes.iter() {
            let (Some(id), Any::Map(f)) = (parse_node(key), node) else { continue };
            if !f.contains_key("note") {
                unnoted.push(id); // made before nodes had a note
            }
            match any_key(f.get("key")) {
                Some(k) => highest = highest.max(k),
                None => {
                    missing.insert(id);
                }
            }
        }
        if missing.is_empty() && unnoted.is_empty() {
            return None;
        }
        for id in unnoted {
            self.note_text(&mut txn, id);
        }
        for (offset, id) in missing.into_iter().enumerate() {
            if let Some(Out::YMap(m)) = self.nodes.get(&txn, &id.0.to_string()) {
                m.insert(&mut txn, "key", Any::Number((highest + 1 + offset as u64) as f64));
            }
        }
        Some(txn.encode_update_v1())
    }

    /// Apply an update received from a client or read from storage.
    pub fn apply_update(&self, bytes: &[u8]) -> Result<(), String> {
        use yrs::updates::decoder::Decode;
        let update = Update::decode_v1(bytes).map_err(|e| e.to_string())?;
        self.doc.transact_mut().apply_update(update).map_err(|e| e.to_string())
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.len(&self.doc.transact()) == 0
    }

    /// Three nodes in a chain, so a fresh `demo` graph has something to show.
    /// Fixed ids: a client that reconnects after a restart re-sends its copy of
    /// these nodes, and with random ids every restart would add duplicates.
    pub fn seed_demo(&self) {
        let id = |n: u128| NodeId(Uuid::from_u128(0x0000_0000_0000_7000_8000_0000_0000_1000 + n));
        let (a, b, c) = (id(1), id(2), id(3));
        self.create_node(a, "design", Place::at_origin(Vec2::new(0, 0)));
        self.create_node(b, "build", Place { anchor: Some(a), off: Vec2::new(240, -40) });
        self.create_node(c, "ship", Place { anchor: Some(b), off: Vec2::new(240, 80) });
        self.create_edge(b, a, EdgeTypeId::DEFAULT);
        self.create_edge(c, b, EdgeTypeId::DEFAULT);
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    /// Encode the whole document state, for a new client or for persistence.
    pub fn encode_state(&self) -> Vec<u8> {
        self.doc.transact().encode_state_as_update_v1(&StateVector::default())
    }
}

fn read_any(m: &MapRef, txn: &impl ReadTxn, key: &str) -> Option<Any> {
    match m.get(txn, key) {
        Some(Out::Any(a)) => Some(a),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sync(from: &Graph, to: &Graph) {
        to.apply_update(&from.encode_state()).unwrap();
    }

    fn keys(g: &Graph) -> Vec<(String, Option<u64>)> {
        let mut v: Vec<_> = g.snapshot().nodes.values().map(|n| (n.title.clone(), n.key)).collect();
        v.sort_by_key(|(_, k)| *k);
        v
    }

    #[test]
    fn nodes_round_trip_into_layout() {
        let g = Graph::new();
        let (a, b) = (NodeId::new(), NodeId::new());
        g.create_node(a, "design", Place::at_origin(Vec2::new(10, 10)));
        g.create_node(b, "build", Place { anchor: Some(a), off: Vec2::new(200, 0) });

        let pos = waypoint_layout::derive_positions(&g.places());
        assert_eq!(pos[&a], Vec2::new(10, 10));
        assert_eq!(pos[&b], Vec2::new(210, 10));
    }

    #[test]
    fn state_syncs_to_another_doc() {
        let g = Graph::new();
        g.create_node(NodeId::new(), "x", Place::at_origin(Vec2::new(1, 2)));
        let other = Graph::new();
        sync(&g, &other);
        assert_eq!(other.places(), g.places());
    }

    #[test]
    fn keys_are_assigned_in_creation_order_and_only_once() {
        let g = Graph::new();
        for t in ["a", "b", "c"] {
            g.create_node(NodeId::new(), t, Place::at_origin(Vec2::ZERO));
        }
        assert!(g.assign_keys().is_some());
        assert_eq!(keys(&g), vec![("a".into(), Some(1)), ("b".into(), Some(2)), ("c".into(), Some(3))]);
        assert!(g.assign_keys().is_none(), "nothing left to number");
    }

    #[test]
    fn keys_survive_renames() {
        let g = Graph::new();
        let id = NodeId::new();
        g.create_node(id, "old name", Place::at_origin(Vec2::ZERO));
        g.assign_keys();
        g.apply_writes(&[Write::SetTitle { id, title: "new name".into() }]);
        assert_eq!(keys(&g), vec![("new name".into(), Some(1))]);
    }

    #[test]
    fn keys_are_never_reused_after_a_delete() {
        let g = Graph::new();
        let (a, b) = (NodeId::new(), NodeId::new());
        g.create_node(a, "a", Place::at_origin(Vec2::ZERO));
        g.create_node(b, "b", Place::at_origin(Vec2::ZERO));
        g.assign_keys();
        g.apply_writes(&[Write::DeleteNode { id: b }]); // had key 2
        g.create_node(NodeId::new(), "c", Place::at_origin(Vec2::ZERO));
        g.assign_keys();
        assert_eq!(keys(&g), vec![("a".into(), Some(1)), ("c".into(), Some(3))], "2 stays retired");
    }

    #[test]
    fn nodes_added_concurrently_by_two_clients_get_distinct_keys() {
        let server = Graph::new();
        let (alice, bob) = (Graph::new(), Graph::new());
        for i in 0..3 {
            alice.create_node(NodeId::new(), &format!("alice {i}"), Place::at_origin(Vec2::ZERO));
            bob.create_node(NodeId::new(), &format!("bob {i}"), Place::at_origin(Vec2::ZERO));
        }
        sync(&alice, &server);
        sync(&bob, &server);
        server.assign_keys();
        let mut got: Vec<u64> = keys(&server).into_iter().map(|(_, k)| k.unwrap()).collect();
        got.sort();
        assert_eq!(got, vec![1, 2, 3, 4, 5, 6], "unique and contiguous");
        // The clients then receive the server's numbering.
        sync(&server, &alice);
        assert_eq!(keys(&alice).len(), 6);
        assert!(keys(&alice).iter().all(|(_, k)| k.is_some()));
    }

    #[test]
    fn an_edge_is_identified_by_its_ordered_pair() {
        let g = Graph::new();
        let (a, b) = (NodeId::new(), NodeId::new());
        g.create_node(a, "a", Place::at_origin(Vec2::ZERO));
        g.create_node(b, "b", Place::at_origin(Vec2::ZERO));
        g.create_edge(a, b, EdgeTypeId::DEFAULT);
        g.create_edge(a, b, EdgeTypeId::DEFAULT); // repeating is a no-op
        assert_eq!(g.snapshot().edges.len(), 1);
        g.create_edge(b, a, EdgeTypeId::DEFAULT); // the other direction is a different edge
        assert_eq!(g.snapshot().edges.len(), 2);
        g.apply_writes(&[Write::Unlink { from: a, to: b }]);
        assert_eq!(g.snapshot().edges, vec![EdgeRec { from: b, to: a, type_id: EdgeTypeId::DEFAULT }]);
        g.create_edge(a, b, EdgeTypeId::DEFAULT); // and it can come back
        assert_eq!(g.snapshot().edges.len(), 2);
    }

    #[test]
    fn edges_stored_under_the_old_key_spelling_are_still_understood() {
        let g = Graph::new();
        let (a, b) = (NodeId::new(), NodeId::new());
        g.create_node(a, "a", Place::at_origin(Vec2::ZERO));
        g.create_node(b, "b", Place::at_origin(Vec2::ZERO));
        // An edge as an older client wrote it: the type is part of the key.
        {
            let mut txn = g.doc.transact_mut();
            g.edges.insert(
                &mut txn,
                format!("{}>{}:{}", a.0, b.0, EdgeTypeId::DEFAULT.0),
                MapPrelim::from([
                    ("from".to_string(), Any::from(a.0.to_string())),
                    ("to".to_string(), Any::from(b.0.to_string())),
                    ("type".to_string(), Any::from(EdgeTypeId::DEFAULT.0.to_string())),
                    ("deleted".to_string(), Any::from(false)),
                ]),
            );
        }
        assert_eq!(g.snapshot().edges.len(), 1);
        g.create_edge(a, b, EdgeTypeId::DEFAULT); // new spelling for the same pair
        assert_eq!(g.snapshot().edges.len(), 1, "one edge, not two");
        g.apply_writes(&[Write::Unlink { from: a, to: b }]);
        assert!(g.snapshot().edges.is_empty(), "removing it removes both entries");
    }

    #[test]
    fn deleting_a_node_hides_its_edges() {
        let g = Graph::new();
        let (a, b) = (NodeId::new(), NodeId::new());
        g.create_node(a, "a", Place::at_origin(Vec2::ZERO));
        g.create_node(b, "b", Place::at_origin(Vec2::ZERO));
        g.create_edge(a, b, EdgeTypeId::DEFAULT);
        g.apply_writes(&[Write::DeleteNode { id: b }]);
        let snap = g.snapshot();
        assert_eq!(snap.nodes.len(), 1);
        assert!(snap.edges.is_empty());
    }
}
