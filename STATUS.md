# Waypoint: current status

Last updated 2026-10-19 (encryption at rest, graph switcher, `create_graph`, PNG copies to clipboard). Design: [DESIGN.md](DESIGN.md). Data model and API:
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
- **Agent API and MCP** (`crates/mcp`, binary `waypoint-mcp`): 26 tools (`connect`, which opens a graph with a key the user gives, several at once via `graph`; `create_graph`, which makes a new graph, opens it and returns its keys; the graph and node reads, `get_full_graph`, `get_types`, create/rename/delete/link/unlink/move, `set_note`/`append_note`, `set_node_type`, `upsert_*_type`, `remove_*_type`, `undo`, `redo`, `apply_batch`). Batches are atomic, max 100 ops, `$N` refers to a node created
  earlier in the batch.
- **Keys** (`crates/server/src/auth.rs`): owner → editor → viewer, each derived
  from the one above by a hash; the graph is identified by its viewer key; a key
  is a bearer credential. Reads need viewer, writes editor; viewers can watch over
  WebSocket but their updates are dropped. Nothing about keys is stored; graphs
  are filed under the hash of the viewer key. See DATA_MODEL.md.
- **Client** (`client/`): PixiJS canvas, pan/zoom, drag with live preview,
  drag moves one node, shift-drag takes what it depends on (`move_node` `with_dependencies`), add/rename/delete nodes, add/remove links, hint bar.
  All edits go through `POST /apply`, the same operations as agents; Ctrl+Z
  undoes and Ctrl+Shift+Z / Ctrl+Y redoes the tab's own batches.
  Asks for a key (or creates a graph), read-only mode for viewers, Share panel.
  A "graphs" button lists the graphs this browser has opened (current one marked) to switch
  between them, forget one, or make a new one. "Export PNG" copies the image to the clipboard
  (when the browser allows) before saving the file.
- **Encryption at rest** (`crates/server/src/crypt.rs`): every stored update and snapshot is
  sealed with XChaCha20-Poly1305 under a key derived from the viewer key; the server keeps
  only the derived cipher in memory while a graph is open, so a copied database is unreadable
  and a graph stays sealed after a restart until someone opens it with a key. Rows from before
  carry `enc = 0` and are re-sealed (then `VACUUM`ed) when their graph is next opened. See
  DATA_MODEL.md, Storage.

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
- **Agent discovery:** `GET /llms.txt` (plain-text API guide), `GET /openapi.json`
  (also `/api/schema`), both public and compiled in from `crates/server/docs/`
  (the OpenAPI file is hand-maintained: update it when ops change). Every error
  body carries `"docs": "/llms.txt"`, and `client/index.html` has an HTML comment
  pointing at the docs.
- **`?key=` on every REST route** (middleware `key_from_query` in
  `crates/server/src/main.rs`) when no `Authorization` header is sent; the
  header wins if both are present.
- **Types** (`crates/model/src/style.rs`, `client/src/style.ts`, `client/src/types.ts`):
  node types (shape: rect/pill/ellipse/hexagon/rhombus, corners: round/square, background + foreground set together, border color and width; no label)
  and edge types (line color, text color, width 1-10, line style, start/end arrow:
  none/arrow/dot/bar, label which may be empty), each a registry in the document (`node_types`, `edge_types`) that
  only holds entries differing from the built-ins (`default`, `depends_on`). Rules, enforced
  by the server and mirrored in the panel: node `fg` on `bg` >= 4.5:1; edge `text_color` >= 4.5:1
  and `color` >= 3:1 against the **canvas colour**, which the owner sets
  (`PUT /background`; edge types that stop reading are recoloured black/white and listed).
  Widths are clamped. Ops `upsert_node_type`, `upsert_edge_type`, `remove_*`, `set_node_type`;
  `create_node.node_type`, `link.edge_type` (relinking retypes). `GET /types`; `/full` carries them.
  Panel: "types" button, top right. Every edge draws its type's label along it, upright.
  Edge types all count as dependencies (cycle checks, layout): the type changes looks only.
- **Theme:** the margin, panels, hint and title follow the canvas colour (`client/src/theme.ts`, CSS variables): a dark canvas gets the dark theme, a light one the light theme. The margin is its own surface: four fixed palettes (two dark, two light), and the closest to the canvas colour with at least 1.15:1 contrast against it is used; 2px border.
- **Peers:** a caller sends `X-Waypoint-Peer` (random id per tab / MCP process) and
  `X-Waypoint-Actor` (free-form name; may repeat). Undo history and the change log's `peer`
  go by id; without an id the name is the id.
- **Cleanup:** repair removes links whose end was deleted (or never existed).
- **Initial layout:** a batch creating 3 or more nodes without `x`/`y` is laid out together (`crates/layout/src/initial.rs`, layered drawing: dependencies left, long links get stand-ins, barycentre sweeps, rows pulled toward neighbours), placed below existing nodes; smaller batches place nodes one by one.
- **Speed:** hover and zoom redraw only the side dots and the camera, not every label; titles are clipped to two lines by cutting text rather than masking. Zoom is proportional to wheel distance. `cargo test -p waypoint-e2e --test perf -- --ignored` times a real graph (`WAYPOINT_PERF_GRAPH=<GET /full dump>`).
- The canvas draws on demand, not every frame (it used a core while idle).

## Verified

- 131 Rust unit tests: 50 layout, 62 server, 10 MCP, 9 model, plus 11 browser tests in
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

- The type panel and rendering (hexagon/ellipse shapes, dashed/dotted borders and lines, dot/bar
  arrowheads) have been type-checked and exercised by one browser test, not looked at.

- `claude mcp add --env WAYPOINT_KEY=<key> waypoint -- "$PWD/target/debug/waypoint-mcp"`
  (the README command) has not been run.

## Not built

- Real identity: peer ids and names are self-declared; nothing says who holds a key.
- Key revocation (derived keys cannot be revoked; rotate by moving to a new graph).
- Durable undo (stacks are in memory, lost on restart), undoing repair writes,
  a log viewer in the UI, restore of deleted nodes from the log.
- Command line.
- Node statuses, sub-graph frames, `Tidy`. Types have no meaning beyond looks; a registry UI for
  merging duplicate type names (concurrent creation of one name hides the later id).

## Known issues and limits

- Anyone who can reach the server can create graphs (no server-level gate).
- The old `demo` graph (and any graph stored under a plain name) is unreachable:
  graphs are now filed under the hash of their viewer key. It is still in
  `waypoint.db`; adopting it under new keys needs a small migration, not written.
- A key in a URL (`?key=` on the WebSocket, and the viewer key that names the graph in every
  REST path) can appear in proxy logs. The viewer key also decrypts the stored graph, so logs
  plus a stolen database defeat encryption at rest. Planned: name graphs in paths by their
  stored id and send the key only in `Authorization` (WebSocket: `Sec-WebSocket-Protocol`).
- Encryption at rest: graphs not yet opened since it shipped are still plaintext in the
  database, and old copies of the database (the host has `data/pre-encryption.db`) stay readable.
  Rolling the server back after graphs were re-sealed leaves them unreadable. Graph ids, blob
  sizes, row counts and creation times are not hidden.
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
- Deployed at https://waypoint.qdat.net (user systemd unit `waypoint.service`
  on the host, listening on 127.0.0.1:3417). The binary is built here with
  `cargo build --profile deploy -p waypoint-server` (full LTO, stripped) and
  uploaded. **How to deploy, rollback and the host layout are in
  `DEPLOY.local.md` in the repo root. It is deliberately not committed**
  (listed in `.git/info/exclude`), so it exists only in the working copy it was
  written in.
- Source is in git at `git@github.com:s3bk/waypoint.git` (`main`).
- Production: `WAYPOINT_STATIC` (built client directory), `WAYPOINT_ADDR`, `WAYPOINT_DB`.
- MCP: `cargo build -p waypoint-mcp`; env `WAYPOINT_URL`
  (default `http://127.0.0.1:3000`), `WAYPOINT_KEY` (required).

## Next steps (proposed order)

0. Layout: build the clustered force-directed layout, the `tidy` op and the label-fit rules into the product. Findings, algorithms and open problems are in [LAYOUT.md](LAYOUT.md); experiment scripts in `tools/layout-experiments/`.
1. Edge-type registry, kinds and statuses, sub-graph frames. Still open from
   notes: hover-highlight between margin and node, per-user inline/hidden modes,
   character-level edits from the UI (it sends the whole text).
