# Waypoint: data model and API

Status: settled design, partly implemented (see DESIGN.md, "Status"), updated 2026-10-17.
Builds on DESIGN.md. Items marked
**(decision)** are my recommendation and need your confirmation.

## Principles

1. One model, one set of operations. The canvas, the command line and the
   agent API all produce the same `Op` values. Nothing mutates the graph
   except by applying an `Op`.
2. The graph is a CRDT document. Ops are applied to it as CRDT transactions,
   so edits merge instead of being rejected. Clients edit locally and sync;
   the server holds the canonical copy, validates, persists and relays.
3. Agent and human edits are indistinguishable except for the `actor` field.

## Sync **(decision)**

CRDT, as you asked. The motivating case is a slow agent: it reads the graph,
thinks for a minute, then writes. With server-ordered ops its write would be
based on a stale revision and be rejected or conflict. With a CRDT its edits
merge into whatever the graph has become, and nobody waits for it.

**Library: Yjs on the client, `yrs` (its Rust port) on the server**
(confirmed). Automerge stays possible later: the document shape below uses
only maps and text, and agents never touch the CRDT directly, so the library
is contained behind the server's document layer.

### Document shape

One Yjs document per graph:

- `nodes`: `Y.Map<NodeId, Y.Map>`; each field (`title`, `status`, `place`,
  `kind`, `tags`, `deleted`) is its own key, so concurrent edits to different
  fields of one node both survive. Same field concurrently: last writer wins.
- `edges`: `Y.Map<EdgeKey, Y.Map>`. **An edge is identified by the ordered
  pair of its ends**, so the key is `"{from}>{to}"` and there is at most one
  edge per ordered pair: two people adding the same edge converge on one
  entry. The edge's type is a field (`type`, the stable id of an entry in
  `edge_types`, not its name), not part of its identity. `A>B` and
  `B>A` are different edges (for an acyclic type, both together would be a
  cycle). Older documents keyed edges `"{from}>{to}:{type_id}"`; readers go by
  the `from` and `to` fields, not the key, so both spellings are understood,
  an edge is live if any entry for its pair is live, and removing an edge marks
  every entry for the pair.
- `node_types`, `edge_types`: `Y.Map<TypeId, Y.Map>`, the type registries. Each
  field of an entry is its own key, so edits merge field by field. Only entries
  that differ from the built-ins are stored (see "Node and edge types").
- `meta`: `title` and `background` (both owner-only; the server writes them
  back if another client changes them).
- `log`: `Y.Array` of change-log entries (`peer`, `actor`, label, ops, time).
- `notes`: `Y.Map<NoteId, Y.Map>` with `body` as `Y.Text`, so two people (or
  an agent and a person) can edit one note at the same time and both edits
  merge.
- Deletion is a `deleted` flag, never key removal. A concurrent edit to a
  deleted node is kept, and restoring is just clearing the flag.
- Presence is not stored in the document; it goes over the awareness channel.

### What a CRDT cannot do, and how we cover it

- **Edges of `acyclic` kinds (e.g. `depends_on`) cannot be enforced at write time.** Two people
  can each add an edge that is fine alone and forms a cycle once merged. The
  server runs a check after each merge. If a cycle appears, it marks the
  most recently added edge in the cycle `conflicted` (a flag in the document,
  so everyone sees the same thing), excludes it from derived values, and
  surfaces it in the UI and in agent responses until someone deletes or
  fixes it. Nothing is silently dropped.
- **Validation.** Writes from the human client go straight into the document,
  so the server cannot refuse them. Instead it validates after merge
  (dangling edge endpoints, unknown status values, bad shapes), repairs what
  it can, and flags the rest. Agents go through the server, which can
  validate before applying.
- **History and audit.** Yjs does not record who did what by itself. The
  server keeps an append-only change log: each accepted update is stored with
  actor, time and a decoded summary of ops. That powers "what did the agent
  change" and the agent `changes` call.
- **Undo** is per actor over their own changes (Yjs `UndoManager` on the
  client; the server applies inverse ops for agent batches).

### Agents do not speak CRDT

Agents use the semantic `Op` API below. The server turns each batch into one
Yjs transaction applied to its current copy of the document and returns the
result. So an agent's write always lands on the latest state; there is no
base revision to go stale. Reads return a `version` token (the document
state vector, opaque to the agent) used only for `changes(since)`.

A write can still be meaningless if the thing it touches changed (the agent
sets status on a node that someone deleted). The server reports that per op
in the result (`applied`, `applied_but_node_deleted`, `no_such_node`); it does
not roll back the batch.

## Ids

UUIDv7 (time-ordered), serialized as strings. Clients may generate ids for
new entities so they can create a node and connect it in one batch without a
round trip. The server rejects duplicates.

### Keys: short, stable names for nodes

A UUID is stable but nobody can type it, so every node also has a **key**, a
small integer shown as `#12`. It is the name people and agents use. Keys are
the answer to "ids that survive renaming": the key is never derived from the
title, so a rename cannot change it.

- Keys are assigned **only by the server**, as the single writer, after each
  merge. Two clients adding nodes at once can therefore never get the same
  number. The assignment numbers nodes that lack a key in id order, which, with
  time-ordered ids, is creation order (clients generate UUIDv7 for this reason).
  Until the server has numbered a node it shows no key.
- The next key is one past the highest key ever seen, over all nodes including
  soft-deleted ones. A key is never reused or changed; deleting #2 retires #2.
- The key is a field (`key`) on the node's map. Clients never write it.
- A node **reference**, as used by the agent API, is `"#12"` or `"12"` (a key),
  a UUID, or `"$N"` for the node created by op N earlier in the same batch.
  Keys cannot name a node created in the same batch because it has none yet,
  hence `$N`.
- Not handled yet: a malicious or buggy client writing `key` itself, and
  duplicate keys if two servers ever number the same graph.

## Entities

```rust
struct Graph {
    id: GraphId,
    name: String,
    // no rev: the CRDT document has no single revision number
}

struct Node {
    id: NodeId,
    graph: GraphId,
    title: String,
    kind: String,            // id of a NodeKind in the graph's config
    status: String,          // id of a Status in the graph's config
    scope: Option<NodeId>,   // sub-graph this node lives in; None = top level
    place: Place,            // (anchor, offset), one atomic value; see Layout
    locked: bool,            // layout never moves this node (you can still drag it)
    expanded: bool,          // container shown inline as a frame around its contents
    tags: Vec<String>,
    created_by: Actor,
    created_at: Timestamp,
    updated_at: Timestamp,
    deleted: bool,           // soft delete, so undo and old refs work
}

struct Place {
    anchor: Option<NodeId>,  // same-scope neighbour; None = scope origin
    off: (i32, i32),         // integer world units from the anchor
}

struct Edge {
    // no id: an edge is identified by (from, to)
    from: NodeId,
    to: NodeId,
    type_id: EdgeTypeId,     // stable id of an entry in edge_types; a field, not identity
    conflicted: bool,        // set by server when it closes a dependency cycle
    created_by: Actor,
}

struct Note {
    id: NoteId,
    node: NodeId,
    author: Actor,
    body: Text,              // collaborative text (Y.Text), Markdown
    created_at: Timestamp,
    updated_at: Timestamp,
    deleted: bool,
}

enum Actor {
    Human { user: UserId },
    Agent { agent: AgentId, on_behalf_of: Option<UserId> },
}
```

### Node and edge types **(implemented)**

Types only change how things look; they have no meaning for the logic. Every
link counts as a dependency (cycle checks, layout) whatever its type. Both
registries live in the document (`node_types`, `edge_types`) as
`Y.Map<id, Y.Map>`, one key per field. An entry points to nothing; nodes carry
`type` (a node type id) and edges carry `type` (an edge type id). Names are
unique per kind: if two ids end up with one name, the earlier id keeps it and
the later one is hidden. A rename is one field write.

Only entries that differ from the built-ins are stored. The built-ins are the
node type `default` (id `...0101`) and the edge type `depends_on` (id
`...0001`); they cannot be removed. Removal of any other type sets a `removed`
flag; nodes/edges using a missing or removed type fall back to the built-in.
Readers sanitize every field and apply fallbacks, so a bad write from any
client cannot break rendering.

**Node type:** `name`, `shape` (rect, pill, ellipse, hexagon, rhombus),
`corners` (round or square; used by rect, hexagon, rhombus), `bg` and `fg`
(set together; contrast at least 4.5:1), `border` color, `border_width`
(clamped 0-10, always solid). A legacy shape `rounded` reads as rect with round
corners.

**Edge type:** `name`, `label` (may be empty, at most 40 chars; empty draws
nothing), `color` (at least 3:1 against the canvas), `text_color` (at least
4.5:1 against the canvas), `width` (clamped 1-10), `line_style` (solid, dashed,
dotted), `start` and `end` arrows (none, arrow, dot, bar).

**Canvas background** (`meta.background`, default `#14161a`) is set only by
the owner (`PUT /background`). Edge types that no longer read on the new
background are recolored black or white and listed in the reply, so a change of
background never blocks.

Ops: `upsert_node_type`, `upsert_edge_type` (only the changed fields are
written), `remove_node_type`, `remove_edge_type`, `set_node_type`;
`create_node.node_type`, `link.edge_type` (linking an existing pair with a new
type retypes it). Each pair `(from, to)` has at most one edge, so two nodes are
joined in a given direction by one type only.

### Sub-graphs **(decision)**

There is no parent/child edge. Grouping is done with sub-graphs: any node can
contain a graph of its own, like a feature that expands into its tasks.

- One Yjs document per project. A sub-graph is not a separate document; it is
  the set of nodes whose `scope` equals the containing node's id. `scope =
  None` is the top level.
- Each scope has its own canvas: positions are relative to the scope's
  origin (the container's position). The UI opens a node to enter its sub-graph and shows a breadcrumb
  back up. Nesting can go any depth.
- Edges are drawn between nodes in the same scope. An edge between nodes in
  different scopes is allowed (a task deep inside "auth" can depend on a
  top-level "database" node); the UI shows it attached to the nearest
  visible ancestor of the hidden end.
- A container node's own `status` is still manual by default. Derived
  roll-ups (done when all contained nodes are done, progress as a fraction)
  are computed values, never stored.
- Moving a node into another scope is `UpdateNode { scope }`. Deleting a
  container soft-deletes its contents with it; restoring brings them back.
- A scope cannot contain itself or a descendant (checked after merge like
  cycles; offender flagged `conflicted`).
- Agents: `subgraph(root, depth)` and `neighbors` take an optional `scope`;
  `get_node` reports `scope` and whether the node has contents.

### Layout: anchored positions, dragging, and making room **(decision)**

Positions are shared state (every human and agent sees the same canvas), so
layout is a data problem, not just a rendering one. Positions are stored
**relative to a neighbour**, so that moving or making room for a group
usually costs one write instead of one per node.

**1. Representation.** Each node stores one value, `place = (anchor, off)`:
- `anchor` is another node in the same scope, or `None` for the scope origin.
- `off` is an integer `(dx, dy)` offset from the anchor's position.
- A node's position is `pos(anchor) + off`, and `pos(None)` is the scope
  origin. Integer sums are exact, so every client derives identical positions.
  Derived positions are cached and recomputed only for the affected subtree.
- `place` is a single atomic value (one Yjs key holding the pair). An offset
  only makes sense relative to its anchor, so a concurrent re-anchor and
  offset change must never mix: the whole pair is last-writer-wins.
- The anchors form a forest rooted at the scope origin. It is internal
  structure. The agent API and the command line speak in absolute
  coordinates; reads return the derived `pos`.
- Nodes have a fixed footprint (default 160x56 world units, same for every
  node and kind) so every client agrees on what overlaps what. Notes are
  overlays and take no layout space. Variable widths can come later as a
  deterministic size class stored on the node.
- Edges are not pushed around by make-room, but they are obstacles for the
  node being placed (see "Edges as obstacles" below). Edges are drawn straight
  between node borders; curved routing is a later, render-only problem.

**2. Choosing and keeping anchors.** The invariant, kept by the layout crate:
the anchor of a node is a *neighbour* in the same scope, that is, joined to it
by a live edge of any type or direction. If it has no neighbour it is anchored
to the origin.
- A new node anchors to the node it was created from or linked to; otherwise
  to the origin.
- A node that later gains an edge keeps its anchor. Its offset already holds
  its position, so re-anchoring would change nothing visible.
- When an edge is deleted, or a node is deleted, each node left with an
  invalid anchor is re-anchored in the same transaction: to its nearest
  remaining neighbour that is not one of its own descendants (ties by id),
  or to the origin. The offset is recomputed so nothing moves. Cost: one
  write per affected node, usually one or two.
- Position derivation never requires the anchor edge to exist, only the
  anchor node, which is soft-deleted rather than removed. A stale anchor
  therefore never makes a node jump; it just waits for repair.
- Concurrent re-anchors can form an anchor cycle after a merge. Until
  repaired, derivation treats the members of a cycle as anchored at the
  origin. The server (the single repair writer) then re-anchors the most
  recently changed member to the origin or a valid neighbour, using the
  positions from before the merge so nothing jumps.
- On drop (rule 3) a node may re-anchor to a much nearer neighbour so that
  subtrees stay spatially local.

**3. Dragging.**
- While dragging, the client moves the node locally and sends its position
  over the awareness channel (ephemeral, not in the document), so others see
  it move live without filling the document history with every frame.
- On drop the client commits one transaction (one undo step). The default
  drag takes the node's whole anchor subtree (its dependants): one write, the
  node's own `off`. Holding Shift (switchable mid-drag) moves only the
  dragged node: its own `off` changes, and each node anchored directly to it
  has its `off` reduced by the same delta so it stays put. That costs 1 + the
  number of direct dependants; a hub with many dependants costs more.
- The anchor tree is hidden structure, and in a DAG "who follows whom" is not
  obvious. So the UI must make the mode visible: while dragging, the nodes
  that will move are outlined, and a hint shows the current mode and how to
  switch. The default (sub-graph or single node) is a client preference; the
  layout crate supports both through `move_node(.., with_subtree)`.
- Dragging a selection: the selected nodes whose anchor is not selected get
  the delta; nodes anchored inside the selection follow; unselected direct
  dependants of selected nodes are compensated.
- Optional snap to a grid or to alignment with neighbours, a client setting.
- A dropped node wins: if it lands on others, they move out of the way.

**4. Where a new node goes (placement).** (`add_node` in the layout crate.) In priority order:
1. An explicit position (clicked or dropped on the canvas). It is converted
   to a `place` by choosing an anchor (the node it is linked to, else the
   nearest neighbour, else the origin) and setting `off`.
2. A `near` hint, or, if the node is created together with an edge, next to
   its connected node in the flow direction. Flow direction is a graph
   setting, default left to right with prerequisites on the left: a node that
   depends on X is placed to X's right.
3. Otherwise the nearest free spot to the centre of the current view (for the
   command line) or to the scope's content (for agents, who have no view).

**5. Making room.** After a node is placed or dropped, nodes whose footprint
(plus a margin) overlaps it are pushed away along the shortest direction out
of overlap. Pushing a node moves its whole anchor subtree rigidly, so a
cluster keeps its shape. The shifted nodes are expressed as writes only for
subtree roots: a node whose anchor is also moving by the same amount needs no
write. If a moved subtree now overlaps something else, that is pushed in turn,
rippling outward. Properties:
- Cheap: writes equal the number of subtree roots moved, not the number of
  nodes moved. A write budget (e.g. 50) caps the ripple; if it runs out the
  remaining overlap is left and flagged for a tidy.
- Local and mental-map friendly: only things in the way move, and the left or
  right and above or below order of nodes is kept, so nothing swaps sides.
- `locked` nodes are never pushed, and neither are their subtrees: a locked
  node is a boundary, and its `off` is compensated (one write) when its anchor
  moves. Others route around it.
- Deterministic: same input state gives the same output, with ties broken by
  node id, so server and client results agree and can be tested.
- All writes go in the same transaction as the creation or drop, so peers see
  one coherent change and animate it. Animation is a client-side tween from
  old to new position; no animation state is stored.

**Edges as obstacles.** A node that is dropped, or added, must not end up on top
of a link between two other nodes. After a drop or an add, any node in the
moved group that lies on an edge not touching the group is moved straight
across that edge, to the side it is already on, by just enough to clear it
(the node's half-extent along the edge's normal plus a 16-unit clearance).
This repeats up to six times in case clearing one edge lands on another.
- The dropped node yields to edges; then everything else yields to the
  dropped node (make-room runs after).
- A sub-graph drag moves the whole group together; only edges with no end in
  the group count, since the group's own links move with it.
- A node's own links never push it away.
- The check uses the line as drawn: from the border of one node to the border
  of the other, matching the client.
- Nodes pushed aside by make-room are not checked, so they can still land on
  an edge. Tidy will handle that.

**6. Concurrent edits.** Two people can add nodes into the same gap at once,
each pushing neighbours, and the merge may leave overlaps. After each merge
the server checks the nodes touched by that merge for overlap and runs rule 5
for them, as the single writer, so clients never fight over repairs. It is
logged as a layout change.

**7. Whole-graph layout (tidy).** A separate, explicit action, `Tidy { scope,
nodes? }`. It runs a layered layout along the flow direction (layers from the
`blocks` edge types, ordering to reduce crossings) and skips `locked` nodes.
It also rebuilds the anchors: each node is anchored to its primary neighbour in
the previous layer, so the result is a clean tree for later edits. It writes
every node in one batch, is never automatic (it discards the user's
arrangement), and is one undo step. It is what an agent calls after building a
large graph. It also compacts the space left when containers collapse.

**8. Sub-graphs move as one.** A container node is either collapsed (a normal
node with the fixed footprint) or expanded (drawn as a frame containing its
sub-graph on the same canvas).
- The scope origin of a sub-graph is the container's position. Children's
  anchors are in the same scope, with `None` meaning the container's origin,
  so moving a container moves its whole contents with one write, however
  large the sub-graph. Nesting composes.
- An expanded container's footprint is the bounding box of its contents and
  its origin, plus padding and a title bar, computed from derived positions by
  a pure function, so every client gets the same rectangle. Collapsed it is
  the fixed footprint.
- In make-room and tidy a container is one rigid body: pushing it moves it
  as a unit and its contents ride along. Siblings are pushed by its frame,
  never by individual children.
- Adding a node inside an expanded container runs make-room within that scope
  first. If the contents outgrow the frame, the frame grows (it is derived),
  and its overlap with siblings is resolved by a second pass in the parent
  scope, both in the same transaction.
- Dragging a node from one frame into another changes its `scope`. Its
  `place` is rebuilt for the new scope (anchor = nearest neighbour there or
  the origin, offset set so it lands under the cursor), and nodes that were
  anchored to it in the old scope are re-anchored and compensated. One
  transaction.
- Cross-scope edges follow their endpoints. If an endpoint is in a collapsed
  container, the edge attaches to the container. Cross-scope edges never
  serve as anchors.
- `expanded` is shared node state, not a per-user view setting. The layout
  depends on footprints, and footprints are shared, so everyone must agree on
  what is expanded. A person can still *enter* a sub-graph as its own
  full-screen canvas (a per-user camera choice) without changing it.
- Expanding changes the footprint and triggers make-room for the container.
  Collapsing leaves the space (no auto-pull-back; tidy compacts it).
- `locked` on a container locks the frame's position only; children inside
  remain draggable.

**One implementation.** Anchoring, placement, ripple, repair and tidy live in
a single Rust crate. The server uses it directly (agents, merge repair); the
client uses the same crate compiled to WASM (humans, so drag-drop and add
feel instant). That avoids two implementations drifting apart in behaviour.
Confirmed: WASM for the client.

**Initial layout (implemented).** A batch with at least 3 `create_node` ops that give
no `x`/`y` skips the one-by-one placement and lays the new nodes out together at the
end (`layout::layered`): layers by longest dependency path (dependencies left, one
column per layer), invisible stand-ins on long links, barycentre sweeps keeping the
ordering with the fewest crossings, then rows pulled toward neighbours without
overlap. Each node is anchored to its nearest dependency in the batch; nodes with no
dependency sit at the origin of the new drawing, which goes below everything already
in the graph (or at the origin of an empty one). Explicit `x`/`y` are respected.

**Layout changes in history.** Position writes are tagged `layout` in the
change log so "what did the agent change" can hide them and show only
meaning changes (nodes, edges, notes, status). Undoing a batch restores the
places of moved nodes along with it.

### Per-graph config: kinds and statuses **(decision, not built)**

Not implemented. The per-graph registries that exist are `node_types` and
`edge_types` (above); the `config` map described here is the plan for statuses
and kinds, which would sit beside them.

Node kinds and statuses are defined per graph, in a `config` map inside the
document (so they sync and are edited like anything else). A new graph is
seeded with defaults, which the user can rename, recolor, add to or remove.

```rust
struct StatusDef {
    id: String,        // stable key stored on nodes, e.g. "todo"
    label: String,     // shown to people, renamable without touching nodes
    category: StatusCategory, // Open | Active | Blocked | Done | Dropped
    color: String,
}

struct NodeKindDef {
    id: String,        // "task"
    label: String,
    color: String,
    icon: Option<String>,
}
```

The `category` is what the server's logic reads; the id and label are free.
That is how "done" can be called "shipped" in one graph and "merged" in
another while `ready` still works.

Defaults:
- Statuses: `todo` (Open), `active` (Active), `blocked` (Blocked), `done`
  (Done), `dropped` (Dropped).
- Kinds: `feature`, `task`, `milestone`, `question`.

Rules:
- Nodes store the id, so renaming a label never touches nodes.
- Removing a definition that nodes still use does not delete or rewrite those
  nodes (a concurrent edit may have just set it). They keep the dangling id,
  render with a neutral "unknown" style, and the server flags them in
  validation until someone remaps them. Status falls back to category
  `Open` for derived values.
- There must always be at least one status in each of Open and Done; the
  server re-adds the defaults if a merge empties them.
- Agents: `get_graph` returns the config, so an agent learns the valid
  statuses and kinds. `apply` rejects an unknown kind or status for an agent
  (the server can validate before applying); humans' direct edits can only
  pick from the config in the UI, and anything that slips through merges is
  flagged as above.

### Derived values (computed, never stored)

- `ready`: status category `Open` and every dependency in category `Done`
  (or `Dropped`).
- `blocked_by`: dependencies not yet in category `Done` or `Dropped`.
- Topological layers, critical path, and progress per container.

Status category `Blocked` is a manual flag for external blockers ("waiting on
legal"). Dependency-blocking is derived and shown separately.

### View state is not in the model

Note display mode, camera, selection and margin layout are per-user client
state (DESIGN.md). Node placement is shared because humans and agents arrange the
same canvas; agents may omit a position and the server places the node and makes
room (see Layout). Reads return absolute derived `pos`.

## Operations

The agent-facing and command-line-facing vocabulary. Humans' canvas edits
write to the Yjs document directly; both paths produce the same document
changes and the same change-log entries.

**Implemented today** (`waypoint_model::Op`, planned by the server's `ops`
module): `create_node {title, depends_on?, dependants?, x?, y?}` (`x`,`y`: top-left corner, used only when there are no dependencies or dependants), `rename_node`,
`delete_node`, `link {from, to}`, `unlink {from, to}`,
`move_node {node, x, y, with_dependencies?}` (alias `with_subtree`; moves the node plus everything it depends on, transitively), `set_note {node, body}`, `append_note {node, text}` (each node has one shared note). Nodes are named by reference (see
"Keys"); links by the ordered pair of their ends. The rest of the vocabulary
below (kinds, statuses, scopes) is still to build. Also implemented: `set_node_type`,
`upsert_node_type`, `upsert_edge_type`, `remove_node_type`, `remove_edge_type`,
`create_node.node_type`, `link.edge_type`.

**A batch is all or nothing.** The server plans every op against a snapshot,
using the layout crate for placement, make-room, re-anchoring and joining
links, and applies the result as one document transaction only if all ops
succeeded. A failure reports the index of the failing op and changes nothing.
(This replaces the earlier idea of per-op outcomes with partial application:
an agent that gets "op 2 failed, nothing changed" can fix and resend, with no
half-applied state to reason about.) Batches are limited to 100 ops. Server-made
edits are serialised by a per-graph lock and broadcast to connected clients
like anyone's edits.

```rust
enum Op {
    CreateNode { id, title, kind, status?, pos?, near?, tags? },  // pos absolute in scope; none: placed + room made
    UpdateNode { id, title?, kind?, status?, scope?, locked?, expanded?, tags? },
    MoveNodes { moves: Vec<(NodeId, (i32, i32))>, with_subtree: bool },   // absolute targets; layout-tagged
    Tidy { scope, nodes? },       // run whole-scope layout; layout-tagged
    DeleteNode { id },            // soft delete; links hidden; chains through it bridged; see below
    RestoreNode { id },

    // implemented as UpsertNodeType/UpsertEdgeType/Remove*/SetNodeType; see "Node and edge types"
    UpsertEdgeType { def },           // create or change a registry entry
    RenameEdgeType { id, name },      // one field write; atomic
    RemoveEdgeType { id },
    UpsertStatus { def }, RemoveStatus { id },   // same for kinds/statuses
    UpsertNodeKind { def }, RemoveNodeKind { id },

    CreateEdge { from, to, edge_type },   // name or id; key derived; idempotent
    DeleteEdge { from, to, edge_type },

    CreateNote { id, node, body },
    EditNote { id, edit: TextEdit },  // see below
    DeleteNote { id },
}

struct Batch {
    graph: GraphId,
    ops: Vec<Op>,
    label: Option<String>,        // "split auth into two tasks"; shown in history
}
```

A batch is one transaction. The result lists the outcome of each op (see
above) and the resulting state of touched nodes.

### Deleting a node

Deleting a node is one transaction (`bridge_edges` and `repair_anchors` in the
layout crate, so the UI and the agent API behave the same):
1. **Chains through it are joined.** For every `D -> node` and `node -> P` of
   the same edge type, add `D -> P`: deleting B from "C depends on B depends
   on A" leaves "C depends on A". Pairs that already have such an edge are
   skipped; edges of different types are never mixed. Joining cannot create a
   cycle, since `D` already reached `P` through the node. A node at the end of
   a chain, or with no links, adds nothing.
2. **Anchors are repaired first.** Nodes anchored to the deleted node are
   re-anchored (rule 2) while it still exists, using the links as they will
   be after the delete, so no position changes.
3. The node gets `deleted = true`; its incident edges are marked deleted.
   Nothing is removed, so a restore is possible later; a restore does not
   undo the joins.

### Titles

A node's `title` is one value, so concurrent renames resolve to the later
write. (Character-level merging, as for notes, is not needed for a one-line
title.) Titles may be empty; the UI shows "untitled". Editing normalises
whitespace and caps the title at 200 characters.

### Note edits

Notes are flat (no threads) and their body is a `Y.Text`, so edits are
merged at character level, anchored to the surrounding text rather than to
absolute offsets. Inserting a line between two existing lines while someone
else edits elsewhere, or even in the same note, cannot desync or corrupt the
note.

Edit forms in `TextEdit`:
- `Insert { at, text }`, `Delete { from, to }`: used by the human editor,
  with positions resolved to Yjs relative positions at the client.
- `InsertAfter { anchor, text }` / `InsertBefore { anchor, text }`: insert
  next to a quoted piece of existing text. Meant for agents.
- `Replace { old, new }`: swap quoted text.

Agents get the text-anchored forms because they work with content, not
offsets, and an offset from a stale read would land in the wrong place. The
server finds `anchor`/`old` in the current text; if it is absent or
ambiguous, that op fails with a message (the batch's other ops still apply).

## API

One definition, two transports. Rust types are the source of truth;
TypeScript types and a JSON schema are generated from them (e.g. `ts-rs` or
`specta`) so the client and agent tool definitions cannot drift.

### Realtime (humans' client): WebSocket

The standard Yjs sync protocol (state-vector exchange, then incremental
updates) plus awareness for presence. The server is just another peer that
also persists, validates and relays. A client that was offline reconnects and
merges.

### Request/response (agents): HTTP+JSON, mirrored as MCP tools

**Implemented** (server routes under `/api/graphs/{graph}`, and the
`waypoint-mcp` binary, which exposes them as MCP tools over stdio and talks to
the server over HTTP, because the server owns the live shared documents):

| HTTP | MCP tool | Returns |
|---|---|---|
| `GET /` | `get_graph` | node and link counts |
| `GET /nodes?q=&limit=` | `search_nodes` | nodes (key, ref, title), by key; `q` matches titles or a key |
| `GET /nodes/{ref}` | `get_node` | the node, its position, `depends_on` and `dependants` |
| `GET /nodes/{ref}/neighbours?direction=&depth=` | `get_neighbours` | subgraph up to depth 5 (max 200 nodes, `truncated` flag) |
| `POST /apply` | `apply_batch` | per-op results, or 422 `{error, op_index}` |
| `GET /full` | `get_full_graph` | the whole graph as JSON: nodes, links, notes, types, background |
| `GET /types` | `get_types` | node and edge types, background |
| `PUT /background`, `PUT /title` | (REST only; owner key) | the new value (background: also the edge types it recolored) |
| (ops via `/apply`) | `set_node_type`, `upsert_node_type`, `upsert_edge_type`, `remove_node_type`, `remove_edge_type` | that op's result |
| `POST /undo`, `POST /redo` | `undo`, `redo` | `{ok:true}`, or 409 if nothing to undo/redo; undoes the caller's own latest batch |
| `GET /dot` | `get_dot` | the graph as Graphviz DOT text; `A -> B` means A depends on B |
| `GET /log` | (UI only) | `{log:[{actor, kind, label, ops, at}]}`, one entry per batch |
| (one-op batches) | `create_node`, `rename_node`, `delete_node`, `link_nodes`, `unlink_nodes`, `move_node` | that op's result |

A caller sends `X-Waypoint-Peer` (a random id per tab or MCP process) and
`X-Waypoint-Actor` (a free-form name, sanitized, at most 40 characters, default
`anonymous`; names may collide). Peers are identified by id: the id keys each
peer's undo stack and the log's `peer` field; the name is only shown. Without an
id the name stands in for it. Neither is a verified identity. Undo stacks live in memory and are lost
when the server restarts; the log is kept in the document. Repair writes are
not undoable.

Every node in a result carries its `key` and `ref` (`"#12"`), so an agent can
name it in the next call. Errors are readable ("no node with key #99 (it may
have been deleted)", "... would create a dependency cycle ..."). The MCP server
returns failures as tool results with `isError`, not protocol errors, so the
model sees the message and can recover. Its `instructions` tell the model the
link direction and the key convention.

**Still to build** from the original plan:

| Call | Returns |
|---|---|
| `get_graph(graph)` | also `version`, config (kinds, statuses), status summary |
| `get_node(id, include_notes?)` | notes too |
| `neighbors(.., edge_types?)`, `edge_types()` | edge-type filters and the registry |
| `path(from, to)` | a dependency path if one exists |
| `search(.., status?, kind?, tag?, ready?, cursor)` | status filters and cursors |
| `ready_nodes()` | nodes whose dependencies are all done (needs statuses) |
| `notes(node?, author?, since_version?)` | notes |
| `changes(since_version, limit)` | change log tail, for "what happened since I last looked" |
| writes: `set_status`, `add_note`, `edit_note`, registry ops | needs the matching features |

Design points for agents:

- Responses are bounded: lists take a `limit`, and subgraph calls report
  truncation rather than returning thousands of nodes.
- Output is compact: keys, titles and link lists by default.
- An agent that reads, waits, then writes is safe: its ops merge into the
  current state. References by key do not go stale (keys never change), but a
  node deleted in the meantime gives a clear error.
- Agent edits arrive in the humans' feed like any other change. Attribution
  (the agent as actor) and "undo this batch" need the change log, which does not
  exist yet; batches already carry an optional `label` for it.
- Access is by key, see "Auth and permissions". Not yet: attributing a change to
  an agent or person (a key says what its holder may do, not who they are).

## Auth and permissions

**(decision, implemented in `crates/server/src/auth.rs`)** A graph has three
keys, each derived from the one above by a one-way hash (SHA-256 with a
per-role domain prefix):

```
owner --hash--> editor --hash--> viewer        wp_owner_<64 hex>, wp_editor_…, wp_viewer_…
```

- **viewer** reads. **editor** reads and changes, and can hand out the viewer
  key. **owner** does the same and can hand out the editor key.
- A key is a bearer credential: whoever holds it has its role, and sharing a key
  shares the role. There are no accounts.
- **The graph is identified by its viewer key**, because every key can derive it.
  A request names the graph by that key in the path and proves its role with its
  own key (`Authorization: Bearer`, or `?key=` on the WebSocket, where browsers
  cannot set headers; with no key at all, the path's viewer key is itself a
  viewer credential). The server derives downward from the key it is given and
  compares, so it stores no keys. `GET /api/session` answers "what does my key
  allow, which graph, and which keys can I share" (the role is read from the
  key's prefix, then verified).
- `POST /api/graphs` returns a new set of keys (256 random bits for the owner
  key). A graph exists once an editor or owner first writes to it; a viewer key
  for a graph that does not exist gets 401, the same as a wrong key.
- Viewers can open the WebSocket and receive everything, but their document
  updates are dropped. Reads need viewer, `apply` needs editor.
- On disk a graph is filed under the SHA-256 of its viewer key, so a copy of the
  database does not hand out access.
- **Consequences:** a key cannot be revoked (to cut someone off, move the content
  to a new graph); anyone who can reach the server can create graphs; a key in a
  WebSocket URL can end up in proxy logs; there is no per-person attribution.

## Storage

SQLite, behind `Store` in the server crate. Implemented:

- Tables: `graphs (id, created_at)`, `updates (seq, graph, data)` as an
  append-only log of Yjs updates, and `snapshots (graph, data)`.
- Every update the document produces is appended, whoever made it (a client,
  or the server's own seeding), by observing the document. Awareness
  (presence, live drag previews) is never stored.
- Writes go through one dedicated thread, so a document change never waits
  on the disk. Reads use a second connection (WAL mode).
- Loading a graph applies its snapshot, then the updates after it. The
  persistence observer is attached only afterwards, so loading does not
  write itself back.
- **Compaction:** after 200 updates the log is folded into a snapshot. The
  order matters and is encoded in `Store::last_seq`: read the highest `seq`
  on disk first, then encode the document, then write the snapshot and delete
  updates up to that `seq` in one transaction. Every update on disk was applied
  to the document before it was written, so the snapshot is guaranteed to
  contain what is deleted; updates that arrive meanwhile have higher `seq` and
  stay.
- Graphs load on first use and stay in memory. A graph id is the SHA-256 (64 hex
  characters) of its viewer key. A graph named `demo` is still seeded with three
  fixed-id nodes when empty, but nothing can reach it any more (see STATUS.md). The ids are fixed because a client reconnecting after a server
  restart re-sends its copy; random ids would duplicate the nodes.
- **Encrypted at rest** (`crypt.rs`): every update and snapshot is sealed with
  XChaCha20-Poly1305 under a key derived from the graph's viewer key, so a copy
  of the database holds no readable graph. The server never stores the key: a
  graph is opened by a request that presents a key (all three roles derive the
  viewer key), and the room keeps only the derived cipher in memory. After a
  restart a graph stays sealed until someone opens it. The graph id and blob
  kind are authenticated, so blobs cannot be moved between graphs. Rows carry an
  `enc` flag; rows from before encryption (`enc = 0`) are read as plaintext and
  replaced by a sealed snapshot when their graph is next opened, followed by a
  `VACUUM` and log truncate (with `secure_delete` on) so the plaintext does not
  linger in free pages. Not hidden: graph ids (hashes), blob sizes, row counts
  and creation times. Anyone holding a key, or the running process, can read.
- Shutdown on Ctrl-C drains the write queue; it exits after two seconds even
  if WebSocket clients stay connected.

Not yet implemented: the append-only change log (actor, time, op summary),
users, agents and tokens. Derived indexes (for `search`, `ready_nodes`) are to
be rebuilt in memory from the document and can be dropped at any time.

**Known weakness: stale clients.** A browser tab that outlives a server restart
(or that was offline) pushes its whole document back in on reconnect. With a
CRDT that merges, so old state can resurface (a moved node jumps back, deleted
nodes reappear if the delete was lost). Mitigation to design: version the
document and have the server refuse or reset clients older than it.

## Decisions settled

- Sync: CRDT, Yjs/`yrs`. Swappable later.
- Edges: types are CRDT registry entries with a stable id and a renamable
  name (rename is one atomic field write); meaning comes from a fully
  editable per-graph registry seeded with defaults (flags: directed, blocks, acyclic). Edges may
  cross scopes.
- Node kinds and statuses: per graph, with a default list; statuses carry a
  category that the server logic uses.
- Grouping: sub-graphs via `scope`, no parent/child edge.
- Layout: each node stores a `place` (anchor neighbour + integer offset);
  moving or making room for a cluster is one write per subtree root.
  Absolute positions are derived and are what the APIs speak.
- One graph (one document) per project; no cross-project links. A graph does
  not need to be connected.
- Notes: flat list per node, collaborative text.
- Storage: SQLite.

## Open questions

None blocking. The data model is settled enough to start. Next: project
skeleton (Vite + PixiJS client, Rust server with `yrs`, SQLite), starting
with the document layer and the agent `apply` path.
