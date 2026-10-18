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
is optional; `WAYPOINT_ACTOR` (default `agent`) labels this agent in the change
log and keeps its undo history apart from other actors. Tools:
`get_graph`, `search_nodes`, `get_node`, `get_neighbours`, `get_conflicts`, `create_node`,
`rename_node`, `delete_node`, `link_nodes`, `unlink_nodes`, `move_node`,
`set_note`, `append_note`, `get_dot`, `apply_batch`. Nodes are named by their key (`#12`, shown on each node in the UI);
"link from -> to" means "from depends on to". Give agents an editor key (or a
viewer key to let them look only), never the owner key.
