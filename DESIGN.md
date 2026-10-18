# Waypoint: design so far

Status: first vertical slice running (see "Status" below). Last updated
2026-10-17. The data model, operations and API are specified in
[DATA_MODEL.md](DATA_MODEL.md); this file holds the overview and the
decisions that shape the whole system.

## What it is

A tool for building project feature/todo dependency graphs. It is
collaborative and real-time. Humans and AI agents work on the same graph.

- **Humans:** a 2D canvas with a command line at the bottom.
- **AI agents:** an API for graph traversal and modification.
- **Notes:** every node can have notes attached.

## Architecture

- **Server:** Rust. Holds the canonical copy of each graph as a CRDT
  document (`yrs`), persists it to SQLite, relays sync to clients, validates
  and repairs after merges, and serves the agent API (HTTP+JSON, mirrored as
  MCP tools).
- **Client:** TypeScript, Vite, PixiJS, Yjs.
- **Shared Rust crates:** the model and operation types, and a layout crate
  (placement, make-room, tidy, repair). The layout crate runs natively on the
  server and as WASM in the client, so human and agent edits behave the same.
  TypeScript types and agent tool schemas are generated from the Rust types.
- **One graph (one document) per project.** Graphs need not be connected.

## Decisions

### Agents (details in DATA_MODEL.md)
- Agents use an HTTP API, exposed as MCP tools by `waypoint-mcp`. They name
  nodes by short key (`#12`), which never changes on rename, and links by the
  ordered pair of their ends ("from depends on to").
- A batch of operations is atomic: planned against a snapshot with the shared
  layout code, then applied as one transaction, or not at all.
- Humans and agents can add nodes at the same moment: keys are assigned only by
  the server, after the merge, so they cannot collide.

### Sync and the model (details in DATA_MODEL.md)
- **CRDT, Yjs/`yrs`** (swappable later). Chosen so a slow agent's edits merge
  into the current graph instead of being rejected as stale.
- Agents never touch the CRDT. They send semantic operations in batches; the
  server applies each batch as one transaction against the current state.
- What a CRDT cannot enforce (no dependency cycles, valid references, unique
  names) is checked by the server after each merge. Problems are repaired or
  flagged, never silently dropped.
- Everything is soft-deleted. The server keeps an append-only change log
  (who, when, what), which powers history, undo per actor, and "what did the
  agent change".
- **Edge types** are registry entries in the document with a stable id and a
  renamable name, so a rename is one atomic write. The registry is fully
  editable and seeded with defaults (`depends_on`, `related`, ...). Each type
  has flags (directed, blocks, acyclic) that give it meaning; nothing is
  hard-coded in the server.
- **Node kinds and statuses** are per graph with default lists. Statuses
  carry a category (Open, Active, Blocked, Done, Dropped) that the server
  logic reads, so labels are free.
- **Sub-graphs** replace parent/child edges: any node can contain a graph of
  its own (a `scope`). Edges may cross scopes.
- **Notes:** a flat list per node, each body a collaborative text, so edits
  merge at character level.

### Layout (details in DATA_MODEL.md)
- Each node stores a `place`: an anchor neighbour plus an integer offset
  (the scope origin if it has no neighbour). Absolute positions are derived.
  Moving a cluster or making room for a new node costs one write per subtree
  root, not one per node.
- Nodes are draggable. A drag takes the node's sub-graph (its dependants)
  along; Shift moves only that node. The UI outlines what will move and
  hints how to switch, because the sub-graph is hidden structure. Dragging is
  previewed live to others without writing to the document; the drop is one
  transaction.
- Adding or dropping a node pushes overlapping nodes out of the way
  (clusters move as rigid bodies, nothing swaps sides, `locked` nodes stay).
  Concurrent adds are repaired by the server. `Tidy` is an explicit
  whole-scope relayout.
- Links are obstacles for the node being placed: a node dropped on a link is
  moved straight across it, off the line. Nodes pushed aside by make-room are
  not checked.
- Deleting a node joins the links through it ("C depends on B depends on A"
  becomes "C depends on A") and keeps every other node where it is.
- Expanded sub-graphs move as one body with their contents.

### Interaction (what the client does today)
- **Canvas:** drag the background to pan, wheel to zoom.
- **Nodes:** each shows its short key (`#12`) in the corner. Click selects. Drag moves the node with its sub-graph; Shift-drag
  moves only that node (switchable mid-drag). While dragging, the nodes that
  will move are outlined and a hint bar says how to switch. Other people's
  drags are previewed live. The drop is one transaction and makes room.
- **Adding:** double-click empty canvas adds a node and opens its title.
  Ctrl+Enter (Cmd+Enter on Mac) on a selected node, or while typing a title,
  adds a dependant to its right and opens its title. Ctrl+N is not used
  because browsers reserve it.
- **Renaming:** double-click a node, or Enter / F2 on the selected node. Enter
  commits, Esc cancels, clicking elsewhere commits. The text box is a DOM
  element positioned from the camera, so it follows pan and zoom.
- **Linking:** hover a node to show its side dots. Drag from a dot onto
  another node to link them; the arrow points from where the drag starts to
  where it stops, and "start depends on stop". Dropping on empty canvas
  creates a new node (the one the start node depends on). The target outlines
  green or red while dragging; self-links, duplicates and dependency cycles
  are refused, with the reason in the hint bar.
- **Removing:** click a link, then Delete, to remove it (nodes anchored through
  it are re-anchored without moving). Delete on a selected node removes the node.
- Esc clears the selection. The hint bar always shows the keys that apply.
- `?graph=<name>` opens another graph; the server creates it on first use.

### Frontend
- The graph (nodes, edges, pan, zoom) is drawn with PixiJS.
- Notes and the command line are DOM elements on top of the canvas, so text
  stays crisp at any zoom and editing, selection and accessibility are normal
  browser behaviour. Note text is never drawn in WebGL.
- DOM overlays follow the canvas camera: a note's screen position is derived
  from the node's world position and the camera transform. Keep that transform
  in one place so the canvas and the overlays cannot disagree.
- The node/edge model is plain data, separate from the renderer. PixiJS only
  reads it and draws. The same model serves the agent API.

### Notes display
A note can be shown in two places:
- **Over its node** (inline card anchored to the node).
- **In a margin** beside the canvas (a DOM column).

Display mode is a per-user view setting, not shared state: `inline`,
`margin` or `hidden` (a count badge on the node). One person's layout does
not rearrange anyone else's screen. Camera, selection and margin layout are
also per user. The only view-like state that is shared is a container's
`expanded` flag, because it changes footprints and so the layout.

Default margin content:
- The selected node's notes, then its direct neighbours' notes while there is
  space.
- Nothing selected: the margin is empty (or shows a hint). It does not fall
  back to everything in view.
- The selected node's notes always show, collapsed to their first lines if
  long. Neighbours are added until the next one would not fit.
- Overflow shows a line such as "+3 more neighbours"; clicking expands it.
- Both incoming and outgoing edges count as direct neighbours, labelled by
  the edge type's label or inverse label ("depends on", "blocks").
- Hovering a margin note highlights its node, and the other way round.
  A leader line from note to node is optional.

Each note records its author (human or agent, and which one), so the margin
can show authorship and filter by it.

## Open questions

- Order of neighbours in the margin. Proposal: dependencies (parents) before
  dependents (children), then by canvas position, top to bottom. Not
  confirmed.
- Command line grammar for the human side.
- Per-person identity and attribution (access is by derived keys, see
  DATA_MODEL.md "Auth and permissions"), and key revocation.
- Edge routing: links are straight lines, and only the placed node avoids
  them. Curved or routed links, and keeping pushed nodes off links, are open.
- Stale clients after a server restart (see DATA_MODEL.md, "Storage").
- Undo and restore for deleted nodes and links; the soft-delete data is
  there but there is no UI or per-actor undo yet.
- Touch input: canvas-level hit testing was chosen so taps work, but the
  hover-only side dots have no touch equivalent.
- Variable node widths (v1 uses one fixed footprint so all clients agree on
  overlaps).
- Scale: PixiJS was chosen for graphs that may reach thousands of nodes;
  check the layout crate's make-room and the post-merge repair at that size.

## Status

Working today (tested: 78 Rust unit tests (42 layout, 27 server, 9 MCP), plus
scripted headless-Chrome runs of the editing flows and of a real MCP client
editing a graph while a browser watches):
- Server: y-protocol WebSocket sync (`/ws/{graph}`), many graphs, SQLite
  persistence with compaction, `/api/graphs`, `/api/graphs/{graph}/positions`.
- Node keys: short stable names (`#12`) assigned by the server, never reused or
  changed by a rename; shown on each node.
- Agent API and MCP server (`crates/mcp`, binary `waypoint-mcp`): search, read,
  neighbours, and atomic batches of create, rename, delete, link, unlink and
  move; changes show up live in every browser.
- Layout crate (native and WASM): derive positions, drag with or without
  sub-graph, make-room, add/drop with edge avoidance, anchor repair, cycle
  check, link joining on delete.
- Client: render, pan/zoom, drag with live preview, add/rename/delete nodes,
  add/remove links, hint bar, hidpi-sharp text.

Not built yet: agent attribution, the command line, notes and the margin, the edge-type registry (only
`depends_on` exists; its id is fixed in code), node kinds and statuses,
sub-graph frames, `Tidy`, server-side validation and post-merge repair
(cycles, anchors, duplicate edge types), the change log and undo.

The MCP server has also been used for real: `create_node` over stdio added a
node to the live `demo` graph and it appeared in the open browser. Registering
it with Claude Code (`claude mcp add`) is still untested.

The browser test scripts live outside the repo for now. A fuller snapshot,
including known issues, is in [STATUS.md](STATUS.md).

## Next steps

1. Move the browser tests into the repo (`client/e2e`, puppeteer-core with the
   system Chrome) so they run with the unit tests.
2. Server-side validation and post-merge repair, which the CRDT design
   depends on: dependency cycles, invalid anchors, overlaps after concurrent
   adds, stale clients.
3. Make the UI use the same operations as agents (the client still edits the
   document directly and has its own copy of some of that logic), then add
   authentication and per-actor attribution/undo for both.
4. Notes and the margin; the edge-type registry; kinds and statuses;
   sub-graph frames; history and undo.
