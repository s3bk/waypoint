//! Shared types for Waypoint: ids, placement, and the operation vocabulary.
//! See DATA_MODEL.md. Everything here is plain data; behaviour lives in the
//! layout and server crates.

pub mod style;

use serde::{Deserialize, Serialize};
pub use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Time-ordered id; clients may generate their own.
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }
    };
}

id_type!(NodeId);
id_type!(EdgeTypeId);

impl EdgeTypeId {
    /// Seeded edge types get fixed ids, the same in every graph (DATA_MODEL.md).
    pub const DEFAULT: EdgeTypeId = EdgeTypeId(Uuid::from_u128(0x0000_0000_0000_7000_8000_0000_0000_0001));
}

/// Fixed node footprint in world units, shared by every node so all clients
/// agree on what overlaps what.
pub const NODE_WIDTH: i32 = 160;
pub const NODE_HEIGHT: i32 = 56;

/// Where a node should sit: top-left corner in world units.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodePosition {
    pub node: String,
    pub x: i32,
    pub y: i32,
}

/// A point or offset in world units, integer so sums are exact.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Vec2 {
    pub x: i32,
    pub y: i32,
}

impl Vec2 {
    pub const ZERO: Vec2 = Vec2 { x: 0, y: 0 };

    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }
}

impl std::ops::Add for Vec2 {
    type Output = Vec2;
    fn add(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x + o.x, self.y + o.y)
    }
}

impl std::ops::Sub for Vec2 {
    type Output = Vec2;
    fn sub(self, o: Vec2) -> Vec2 {
        Vec2::new(self.x - o.x, self.y - o.y)
    }
}

/// Where a node sits: an offset from an anchor neighbour, or from the scope
/// origin when `anchor` is `None`. Stored as one atomic value, because an
/// offset is only meaningful relative to its anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Place {
    pub anchor: Option<NodeId>,
    pub off: Vec2,
}

impl Place {
    pub const fn at_origin(off: Vec2) -> Self {
        Self { anchor: None, off }
    }
}

/// Who made a change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Actor {
    Human { user: String },
    Agent { agent: String, on_behalf_of: Option<String> },
}

/// The agent-facing vocabulary (DATA_MODEL.md, "Operations"). Nodes are named
/// by *reference*, a string that is one of:
/// - `"#12"` or `"12"`: the node's short key, assigned by the server and never
///   reused or changed by renaming;
/// - a node id (UUID);
/// - `"$0"`, `"$1"`, ...: the node created by the op at that index earlier in
///   the same batch, so a batch can create a node and link to it.
///
/// Edges are identified by the ordered pair of their ends, "from" is the source and "to" the destination.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Add a node. It is placed next to the first of its `destinations` (or near
    /// the first of its `sources`, or below the existing content) and the layout
    /// makes room for it. The old names `depends_on` and `dependants` still work.
    CreateNode {
        title: String,
        /// Nodes the new node links to (it is their source).
        #[serde(default, alias = "depends_on")]
        destinations: Vec<String>,
        /// Nodes that link to the new node (it is their destination).
        #[serde(default, alias = "dependants")]
        sources: Vec<String>,
        /// Top-left corner to put the node at when it has no `destinations` and no
        /// `sources`; the layout still makes room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Name of a node type (see `upsert_node_type`); the default type if omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_type: Option<String>,
    },
    RenameNode { node: String, title: String },
    /// Delete a node. Links through it are joined: deleting B from "C -> B -> A"
    /// leaves "C -> A".
    DeleteNode { node: String },
    /// A link from `from` (the source) to `to` (the destination). Refused if it would close a cycle. `edge_type`
    /// names an edge type; an existing link is changed to it.
    Link {
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edge_type: Option<String>,
    },
    Unlink { from: String, to: String },
    /// Move a node so its top-left corner is at `(x, y)`, then make room. With
    /// `with_destinations` the nodes it links to (and what they link to) move along.
    MoveNode {
        node: String,
        x: i32,
        y: i32,
        #[serde(default, alias = "with_dependencies", alias = "with_subtree")]
        with_destinations: bool,
    },
    /// Lay out the whole graph afresh, overwriting every position: nodes of a type are grouped,
    /// no node sits on an unrelated link, edge labels fit and stay clear. `aspect` is the wanted
    /// width / height of the drawing (default 16 / 9).
    Tidy {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aspect: Option<f64>,
    },
    /// Set every node's position at once (top-left corners), e.g. a layout the person previewed
    /// and accepted. Every live node must be listed. Anchors are rebuilt from the links.
    Layout { positions: Vec<NodePosition> },
    /// Give a node a type by name; `"default"` goes back to the default type.
    SetNodeType { node: String, node_type: String },
    /// Create a node type, or change the one with this `name`. Only the given
    /// fields change. `bg` and `fg` go together and must contrast.
    UpsertNodeType(style::NodeSpec),
    /// Same for edge types: `color`, `width`, `line_style`, `start`/`end` arrows, `text_color`.
    UpsertEdgeType(style::EdgeSpec),
    /// Create a tag, or change the one with this `name`: a coloured one-glyph symbol for nodes and links.
    UpsertTag(style::TagSpec),
    /// Remove a tag; it disappears from everything that had it.
    RemoveTag { name: String },
    /// Add and remove tags (by name) on a node, or on the link `from` -> `to`. Give `node`, or `from` and `to`.
    SetTags {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<String>,
        #[serde(default)]
        add: Vec<String>,
        #[serde(default)]
        remove: Vec<String>,
    },
    /// Remove a type. Nodes or edges that used it show the default style.
    RemoveNodeType { name: String },
    RemoveEdgeType { name: String },
    /// Replace the node's note (each node has one, shared by everyone, empty
    /// until written). The server turns this into a minimal edit of the shared
    /// text, so a person typing elsewhere in the note is not overwritten.
    SetNote { node: String, body: String },
    /// Add text at the end of the node's note (after a blank line if it has
    /// text). Safer than `set_note` when others may be editing.
    AppendNote { node: String, text: String },
}

/// One atomic transaction of operations: if any op fails, none is applied.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub ops: Vec<Op>,
    #[serde(default)]
    pub label: Option<String>,
}
