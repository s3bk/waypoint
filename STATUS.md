# Waypoint: current status

Last updated 2026-10-18. Design: [DESIGN.md](DESIGN.md). Data model and API:
[DATA_MODEL.md](DATA_MODEL.md). Setup: [README.md](README.md).

## Working

- **Server** (`crates/server`): y-protocol WebSocket sync at `/ws/{graph}`, many
  graphs, SQLite persistence (update log, snapshots, compaction after 200
  updates), HTTP agent API, graceful shutdown.
- **Node keys:** short stable names (`#12`), assigned only by the server after
  merges, never reused, unchanged by rename. Shown on each node.
- **Edges** are identified by the ordered pair `(from, to)`. Type is a field.
  Legacy `from>to:type` keys are still read; removal marks every entry for the
  pair.
- **Layout crate** (native and WASM): anchored positions, drag with or without
  sub-graph, make-room, add/drop with edge avoidance, anchor repair, cycle
  check, link joining on delete.
- **Agent API and MCP** (`crates/mcp`, binary `waypoint-mcp`): 12 tools
  (`get_graph`, `search_nodes`, `get_node`, `get_neighbours`, `get_conflicts`, `create_node`,
  `rename_node`, `delete_node`, `link_nodes`, `unlink_nodes`, `move_node`,
  `apply_batch`). Batches are atomic, max 100 ops, `$N` refers to a node created
  earlier in the batch.
- **Keys** (`crates/server/src/auth.rs`): owner → editor → viewer, each derived
  from the one above by a hash; the graph is identified by its viewer key; a key
  is a bearer credential. Reads need viewer, writes editor; viewers can watch over
  WebSocket but their updates are dropped. Nothing about keys is stored; graphs
  are filed under the hash of the viewer key. See DATA_MODEL.md.
- **Client** (`client/`): PixiJS canvas, pan/zoom, drag with live preview,
  shift-drag for one node, add/rename/delete nodes, add/remove links, hint bar.
  All edits go through `POST /apply`, the same operations as agents; Ctrl+Z
  undoes and Ctrl+Shift+Z / Ctrl+Y redoes the tab's own batches.
  Asks for a key (or creates a graph), read-only mode for viewers, Share panel.

- **Post-merge repair** (`crates/server/src/repair.rs`): after each client
  update and on room load, the server flags the newest edge of every dependency
  cycle `conflicted` (kept, excluded from `Snapshot::edges`, listed in
  `Snapshot::conflicted`), breaks anchor cycles and re-anchors invalid anchors,
  and pushes the newer of two overlapping nodes away (oldest stays).
  Idempotent. The client draws `conflicted` edges dashed red, and ignores them
  for cycle checks.

- **Attribution and undo:** each batch carries an actor (`X-Waypoint-Actor`;
  the client uses a per-tab name, MCP uses `WAYPOINT_ACTOR`). The server keeps a
  change log (`GET /log`) and one yrs `UndoManager` per actor
  (`POST /undo`, `/redo`). `create_node` takes optional `x`, `y`.

- **Notes and margin:** every node has one note, a `Y.Text` created by the
  server and shared by everyone. The client margin (`client/src/margin.ts`,
  right-hand column when a node is selected, viewers included) is a text editor
  bound to it: typing is a minimal Yjs edit sent directly (character-level
  merge, caret kept across remote edits). Neighbours' notes show read-only below
  ("+N more"). Ops `set_note` (server edits only the changed part) and
  `append_note`; MCP tools of the same names; `get_node` returns `note`. Nodes
  with a note show a ✎. Direct typing is not in the change log or undo history.
- **Graph title:** set only by the owner (`PUT /api/graphs/{graph}/title`,
  click the name at the top left); stored in the document (`meta.title`), and
  written back by the server if an editor's client changes it. The browser keeps
  `(title, strongest key)` per graph in localStorage (`waypoint.graphs`, keys in
  the clear); with no current key, the start dialog lists them, takes a pasted
  key, or makes a new graph (which asks for a title).
- **Serving the client:** with `WAYPOINT_STATIC=client/dist` the server also
  serves the built client (`npm run build` in `client/`); SIGTERM shuts it down
  cleanly.
- **For agents:** the page shows REST instructions (`#for-agents` in
  `client/index.html`) until the app loads; `GET /api/graphs/{graph}/dot`
  (MCP `get_dot`) returns Graphviz DOT.
- The canvas draws on demand, not every frame (it used a core while idle).

## Verified

- 100 Rust unit tests: 43 layout, 48 server, 9 MCP, plus 8 browser tests in
  `crates/e2e` (thirtyfour, headless Chrome; `cargo test -p waypoint-e2e`):
  editing, MCP agent alongside a browser, keys and the Share panel, New graph,
  and a conflicted edge being flagged and kept. The editing test ends with
  Ctrl+Z restoring a deleted node; the notes test types in the margin, appends as an agent and checks a viewer; an idle page gets no document updates.
- Repair is tested in Rust with simulated concurrent clients; the browser test
  only forces a cycle through the client API.
- Manual: the user confirmed title editing; this session the user had
  `waypoint-mcp` add node "layout" (`#8`, depends on `#1`) to the `demo` graph
  over stdio, and it appeared live.

## Not verified

- `claude mcp add --env WAYPOINT_KEY=<key> waypoint -- "$PWD/target/debug/waypoint-mcp"`
  (the README command) has not been run.

## Not built

- Real identity: the actor is a self-declared label, not who the holder is.
- Key revocation (derived keys cannot be revoked; rotate by moving to a new graph).
- Durable undo (stacks are in memory, lost on restart), undoing repair writes,
  a log viewer in the UI, restore of deleted nodes from the log.
- Command line.
- Edge-type registry (only `depends_on`, id fixed in code), node kinds and
  statuses, sub-graph frames, `Tidy`.
- Validation of other shapes (dangling endpoints are only hidden, not cleaned up).

## Known issues and limits

- Anyone who can reach the server can create graphs (no server-level gate).
- The old `demo` graph (and any graph stored under a plain name) is unreachable:
  graphs are now filed under the hash of their viewer key. It is still in
  `waypoint.db`; adopting it under new keys needs a small migration, not written.
- A key in a WebSocket URL (`?key=`) can appear in proxy logs.
- The client still carries layout code for the live drag preview; committed
  edits use `/apply`, so the server decides the result.
- A client needs an editor key and a reachable server to edit: edits are no
  longer made offline.
- A stale browser tab can re-merge old state after a server restart and
  resurface deleted or moved nodes.
- Nodes pushed aside by make-room may land on links; make-room is not previewed
  while dragging.
- The right-hand dot can overlap an incoming arrowhead (cosmetic).
- No touch equivalent for the hover-only side dots.

## Running

- Server: `cargo run -p waypoint-server` (`WAYPOINT_ADDR`, `WAYPOINT_DB`,
  default `./waypoint.db`), on 127.0.0.1:3000.
- Client: `cd client && npm run dev`, on 127.0.0.1:5173. `npm run wasm`
  rebuilds the layout WASM.
- Production: `WAYPOINT_STATIC` (built client directory), `WAYPOINT_ADDR`, `WAYPOINT_DB`.
- MCP: `cargo build -p waypoint-mcp`; env `WAYPOINT_URL`
  (default `http://127.0.0.1:3000`), `WAYPOINT_KEY` (required).

## Next steps (proposed order)

1. Edge-type registry, kinds and statuses, sub-graph frames. Still open from
   notes: hover-highlight between margin and node, per-user inline/hidden modes,
   character-level edits from the UI (it sends the whole text).
