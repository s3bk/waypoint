# waypoint-mcp

An [MCP](https://modelcontextprotocol.io) server (stdio) that lets AI agents read and edit a
[Waypoint](https://github.com/s3bk/waypoint) graph. It forwards tool calls to a running
Waypoint server over HTTP, so people with the web UI open see the agent's edits live.

```sh
cargo install waypoint-mcp
claude mcp add --scope user waypoint --env WAYPOINT_URL=https://your-waypoint-server -- waypoint-mcp
```

Then give the agent a graph key (an editor key to change the graph, a viewer key to only look) and
have it call the `connect` tool. Keys are never stored by this program. `WAYPOINT_URL` defaults to
`http://127.0.0.1:3000`; `WAYPOINT_KEY` (optional) opens a graph at startup; `WAYPOINT_ACTOR` names
the agent in the change log.

Full instructions, the tool list and troubleshooting: see the
[Waypoint README](https://github.com/s3bk/waypoint#agents-mcp).

Licensed under MIT OR Apache-2.0.
