# Waypoint

Collaborative, real-time project graphs for humans and AI agents.
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
colour (dark canvas, dark theme). Types are looks only: every link goes
from a source to a destination.

## HTTP API

`GET /llms.txt` is the guide for agents, `GET /openapi.json` the schema. Useful
reads: `GET /api/graphs/{graph}/full` (whole graph as JSON), `/types`, `/dot`,
`/log`. Callers may send `X-Waypoint-Peer` (id, keys undo history and the log)
and `X-Waypoint-Actor` (display name, may collide). Drag moves one node;
Shift-drag also moves everything it links to. Ctrl+Z / Ctrl+Shift+Z undo and
redo your own edits.

## Agents (MCP)

`waypoint-mcp` is a stdio MCP server. It does not hold the graph: it forwards each
tool call to a running Waypoint server over HTTP, so a human with the web UI open
sees the agent's edits live.

### Install

**Agents: check first, install only what is missing.** If you already have `waypoint`
tools (`connect`, `get_graph`, ...), skip to step 4. Otherwise check each step before doing it:

- Registered? `claude mcp list` shows a `waypoint` line. If it does, do not add it again
  (`claude mcp add` fails on a duplicate name); if it is listed but not `Connected`, see
  Troubleshooting.
- Binary present? `command -v waypoint-mcp` (or `ls ~/.cargo/bin/waypoint-mcp`). If it
  prints a path, skip step 1.

You need a Rust toolchain (stable, 1.85 or newer, for edition 2024; get it from
https://rustup.rs) and a Waypoint server to talk to: your own (`cargo run -p
waypoint-server`, on `http://127.0.0.1:3000`) or a hosted one, such as
https://waypoint.qdat.net.

1. Build and put the binary at a fixed path. Either, from a checkout of this repo:

   ```sh
   cargo install --path crates/mcp          # installs ~/.cargo/bin/waypoint-mcp
   ```

   or without a checkout:

   ```sh
   cargo install --git git@github.com:s3bk/waypoint.git waypoint-mcp
   ```

   (To keep it inside the checkout instead: `cargo build --release -p waypoint-mcp`
   gives `target/release/waypoint-mcp`; use that absolute path below.)

2. Register it with your MCP client.

   **Claude Code**, once for you, available in every project (the tool is global; keys are
   per project and given in the conversation, see step 4):

   ```sh
   claude mcp add --scope user waypoint --env WAYPOINT_URL=https://waypoint.qdat.net -- waypoint-mcp
   ```

   Without `--scope`, Claude Code registers it for the current project only (`--scope local`).
   `--scope project` writes it to the project's `.mcp.json` instead (shareable; it holds no
   key). Leave out `--env WAYPOINT_URL=…` for a server on `http://127.0.0.1:3000`. If
   `waypoint-mcp` is not on `PATH`, give the absolute path after `--`.

   **Any other client** that reads JSON config:

   ```json
   { "mcpServers": { "waypoint": {
       "command": "waypoint-mcp",
       "env": { "WAYPOINT_URL": "https://waypoint.qdat.net", "WAYPOINT_ACTOR": "my-agent" }
   } } }
   ```

3. Restart the client, then check: in Claude Code run `claude mcp list` (it should say
   `waypoint ... Connected`) or `/mcp`; the server lists 26 tools.

4. Connect to a graph. Ask the user for a key or share link (the Share panel in the
   web UI makes them), then call the `connect` tool with it. Use an **editor** key to
   let the agent change the graph, a **viewer** key to let it only look, and never
   the owner key. A project can keep its key in its `CLAUDE.md` (an editor or viewer key only)
   so the agent connects at the start of each session. After `connect`, the other tools work. If the tools answer "not
   connected", call `connect` first.

To remove it: `claude mcp remove waypoint` (and `cargo uninstall waypoint-mcp`).

### Troubleshooting

- `failed to connect` / the server does not start: run `waypoint-mcp` in a terminal.
  It prints to stderr and waits for JSON on stdin; Ctrl+D quits. Check the path.
- "cannot reach the Waypoint server": `WAYPOINT_URL` is wrong or the server is down;
  `curl $WAYPOINT_URL/health` should print `ok`.
- "that key was not accepted": the key belongs to another server, or was mistyped.
- A write tool says the key may not write: it is a viewer key; ask for an editor key.
- A tool is missing or fails on an old server: the server must be as new as the
  MCP binary (the type and undo tools need a current server).

### Configuration and behaviour

`WAYPOINT_KEY` is optional. Leave it out (the usual setup, so no key is written into
any config): the user pastes a key or share link into the conversation and the
agent calls `connect`. Keys are never stored by the MCP server, they stay in the
transcript, so after a restart the agent connects again. Several graphs can be open
at once: name each with `connect` (`name`) and pass that name as `graph` to the other
tools (without it the latest is used). The key decides what the agent may do
(an editor or owner key reads and changes; a viewer key only reads, and write
tools return an error). `WAYPOINT_URL` (default `http://127.0.0.1:3000`)
is optional; `WAYPOINT_ACTOR` (default `agent`) is the name shown for this agent
in the change log (names may repeat). Each MCP process also sends a random peer
id, which is what keeps its undo history apart from everyone else's. 26 tools (`connect` and `create_graph`, which makes a new graph and opens it, plus):
`get_graph`, `get_full_graph`, `search_nodes`, `get_node`, `get_neighbours`,
`get_conflicts`, `get_dot`, `get_types`, `create_node`, `rename_node`,
`delete_node`, `link_nodes`, `unlink_nodes`, `move_node` (`with_destinations`
also moves what the node links to; the old `with_dependencies` still works), `set_note`, `append_note`, `set_node_type`,
`upsert_node_type`, `upsert_edge_type`, `remove_node_type`, `remove_edge_type`,
`undo`, `redo`, `apply_batch`. Nodes are named by their key (`#12`, shown on each node in the UI);
"link from -> to" means "from" is the source and "to" the destination. Give agents an editor key (or a
viewer key to let them look only), never the owner key.

## Deploying

The live instance is https://waypoint.qdat.net. `scripts/deploy.sh` builds, uploads and restarts (`--rollback` restores the previous binary). The manual steps (build with
`cargo build --profile deploy`, upload, restart the user systemd unit) are in
`DEPLOY.local.md` in the repo root, which is kept out of git on purpose; ask the
maintainer if you do not have it. Agents: see `/llms.txt` and `/openapi.json` on
the running server.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option. Unless you state otherwise, any contribution you submit is dual licensed as above, with no additional terms.
