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
    pub const DEPENDS_ON: EdgeTypeId = EdgeTypeId(Uuid::from_u128(0x0000_0000_0000_7000_8000_0000_0000_0001));
}

/// Fixed node footprint in world units, shared by every node so all clients
/// agree on what overlaps what.
pub const NODE_WIDTH: i32 = 160;
pub const NODE_HEIGHT: i32 = 56;

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
/// Edges are identified by the ordered pair of their ends, "from depends on to".
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Add a node. It is placed next to the first `depends_on` node (or near
    /// the first of `dependants`, or below the existing content) and the layout
    /// makes room for it.
    CreateNode {
        title: String,
        /// Nodes the new node depends on.
        #[serde(default)]
        depends_on: Vec<String>,
        /// Nodes that depend on the new node.
        #[serde(default)]
        dependants: Vec<String>,
        /// Top-left corner to put the node at when it has no `depends_on` and no
        /// `dependants`; the layout still makes room.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<i32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<i32>,
        /// Name of a node type (see `upsert_node_type`); the default type if omitted.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        node_type: Option<String>,
    },
    RenameNode { node: String, title: String },
    /// Delete a node. Links through it are joined: deleting B from "C depends
    /// on B depends on A" leaves "C depends on A".
    DeleteNode { node: String },
    /// `from` depends on `to`. Refused if it would close a cycle. `edge_type`
    /// names an edge type; an existing link is changed to it.
    Link {
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        edge_type: Option<String>,
    },
    Unlink { from: String, to: String },
    /// Move a node so its top-left corner is at `(x, y)`, then make room. With
    /// `with_dependencies` the nodes it depends on (and what they depend on) move along.
    MoveNode {
        node: String,
        x: i32,
        y: i32,
        #[serde(default, alias = "with_subtree")]
        with_dependencies: bool,
    },
    /// Give a node a type by name; `"default"` goes back to the default type.
    SetNodeType { node: String, node_type: String },
    /// Create a node type, or change the one with this `name`. Only the given
    /// fields change. `bg` and `fg` go together and must contrast.
    UpsertNodeType(style::NodeSpec),
    /// Same for edge types: `color`, `width`, `line_style`, `start`/`end` arrows, `text_color`.
    UpsertEdgeType(style::EdgeSpec),
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
