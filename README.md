# Waypoint

Collaborative, real-time project dependency graphs for humans and AI agents.
Design: [DESIGN.md](DESIGN.md). Data model and API: [DATA_MODEL.md](DATA_MODEL.md).

## Layout

- `crates/model`: shared ids, placement and operation types.
- `crates/layout`: pure layout code (derive positions, later make-room and
  tidy). Runs natively on the server and as WASM in the client.
- `crates/mcp`: `waypoint-mcp`, an MCP server (stdio) that lets AI agents read
  and edit a graph through the server's HTTP API.
- `crates/server`: axum server holding each graph as a `yrs` document, synced
  over WebSocket (`/ws/{graph}`) and persisted to SQLite.
- `client`: Vite, TypeScript, PixiJS, Yjs.
- `crates/e2e`: browser tests with thirtyfour (WebDriver). Each test starts its own
  server, Vite and chromedriver on free ports.

## Develop

```sh
cargo test                      # model, layout, server
cargo test -p waypoint-e2e      # browser tests (headless Chrome via chromedriver; needs `npm install` in client)
cargo run -p waypoint-server    # http://127.0.0.1:3000
                                # WAYPOINT_ADDR: listen address; WAYPOINT_DB: SQLite file (default ./waypoint.db)

cd client
npm install
npm run wasm                    # build the layout crate into src/wasm
npm run dev                     # http://127.0.0.1:5173/
```

## Keys

A graph has three keys, each derived from the one above, so a key lets you
derive the ones below it but never above: **owner** (everything, and can hand out
the editor key) → **editor** (change the graph, and can hand out the viewer key)
→ **viewer** (read only). Sharing a key shares its role. The graph is identified
by its viewer key.

On opening the client you are asked for a key; leave it empty to create a new
graph, and keep the three keys shown (they cannot be recovered). Open
`http://127.0.0.1:5173/#key=<key>` to use a key from a link; the key is
remembered in the browser. The Share button lists the keys your key can hand out
as links, and "New graph" makes another graph with a new owner key (an existing graph's
owner key cannot be re-issued: the others are derived from it). `curl -X POST http://127.0.0.1:3000/api/graphs` also makes a set.
Keys cannot be revoked: to cut someone off, move the content to a new graph.

## Types and theme

Nodes and edges have types that set how they look (the "types" button, top
right; also ops and MCP tools). Node types: shape (rect, pill, ellipse, hexagon,
rhombus), round or square corners, background and text colour (contrast at least
4.5:1), border colour and width. Edge types: line colour, text colour, width
(1-10), solid/dashed/dotted, start and end arrows, label (may be empty). Edge
colours must read against the canvas colour, which only the owner can change
(`PUT /api/graphs/{graph}/background`). The margin and panels follow the canvas
colour (dark canvas, dark theme). Types are looks only: every link is a
dependency.

## HTTP API

`GET /llms.txt` is the guide for agents, `GET /openapi.json` the schema. Useful
reads: `GET /api/graphs/{graph}/full` (whole graph as JSON), `/types`, `/dot`,
`/log`. Callers may send `X-Waypoint-Peer` (id, keys undo history and the log)
and `X-Waypoint-Actor` (display name, may collide). Drag moves one node;
Shift-drag also moves everything it depends on. Ctrl+Z / Ctrl+Shift+Z undo and
redo your own edits.

## Agents (MCP)

Build and register the MCP server with your MCP client; the Waypoint server must
be running, because edits go through it so everyone sees them live.

```sh
cargo build -p waypoint-mcp
# Claude Code, for example:
claude mcp add --env WAYPOINT_KEY=wp_editor_… waypoint -- "$PWD/target/debug/waypoint-mcp"
```

`WAYPOINT_KEY` is required: it opens one graph and decides what the agent may do
(an editor or owner key reads and changes; a viewer key only reads, and write
tools return an error). `WAYPOINT_URL` (default `http://127.0.0.1:3000`)
is optional; `WAYPOINT_ACTOR` (default `agent`) is the name shown for this agent
in the change log (names may repeat). Each MCP process also sends a random peer
id, which is what keeps its undo history apart from everyone else's. 24 tools:
`get_graph`, `get_full_graph`, `search_nodes`, `get_node`, `get_neighbours`,
`get_conflicts`, `get_dot`, `get_types`, `create_node`, `rename_node`,
`delete_node`, `link_nodes`, `unlink_nodes`, `move_node` (`with_dependencies`
also moves what the node depends on), `set_note`, `append_note`, `set_node_type`,
`upsert_node_type`, `upsert_edge_type`, `remove_node_type`, `remove_edge_type`,
`undo`, `redo`, `apply_batch`. Nodes are named by their key (`#12`, shown on each node in the UI);
"link from -> to" means "from depends on to". Give agents an editor key (or a
viewer key to let them look only), never the owner key.

## Deploying

The live instance is https://waypoint.qdat.net. `scripts/deploy.sh` builds, uploads and restarts (`--rollback` restores the previous binary). The manual steps (build with
`cargo build --profile deploy`, upload, restart the user systemd unit) are in
`DEPLOY.local.md` in the repo root, which is kept out of git on purpose; ask the
maintainer if you do not have it. Agents: see `/llms.txt` and `/openapi.json` on
the running server.
