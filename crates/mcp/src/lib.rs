//! An MCP (Model Context Protocol) server that lets an AI agent read and edit a
//! Waypoint graph. It speaks JSON-RPC over stdio and forwards each tool call
//! to the Waypoint server's HTTP API, so edits reach everyone connected to the
//! graph live, exactly like a person's.
//!
//! Nodes are named by *reference*: a short key like `"#12"` (assigned by the
//! server, stable across renames), an id, or `"$0"` for a node created earlier
//! in the same batch. Links are identified by the ordered pair of their ends:
//! "from links to" to.

use serde_json::{Value, json};

/// The versions of the protocol this server can speak, newest last.
const SUPPORTED_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

/// Where tool calls go. The real one is HTTP; tests use a recording fake.
pub trait Backend {
    fn get(&self, path_and_query: &str) -> Result<Value, String>;
    fn post(&self, path: &str, body: &Value) -> Result<Value, String>;
    /// GET a plain-text reply (such as DOT); it comes back as a JSON string.
    fn get_text(&self, path: &str) -> Result<Value, String>;
    /// Open another graph with `key`, calling it `name` (default: its title). Returns `{name, graph, role, title}`, or why the key was refused.
    fn connect(&self, _key: &str, _name: Option<&str>) -> Result<Value, String> {
        Err("this backend cannot change keys".into())
    }
    /// Choose the graph the next requests go to: the one called `graph` (a name or id), or with
    /// `None` the most recently connected. `Ok(None)` means the backend has no connections of its own.
    fn select(&self, _graph: Option<&str>) -> Result<Option<String>, String> {
        Ok(None)
    }
}

/// The key inside what a person pasted: a bare key, or a share link with `#key=` / `?key=`.
pub fn extract_key(text: &str) -> Option<String> {
    let start = text.find("wp_")?;
    let key: String = text[start..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
    (key.len() > 3).then_some(key)
}

pub struct Config {
    /// The graph the key opens, identified by its viewer key (the server tells us). Empty when
    /// no key was configured; the user then gives one with the `connect` tool.
    pub graph: String,
}

const INSTRUCTIONS: &str = "\
Waypoint holds a graph of project work items (nodes) joined by links.\n\
A link `from -> to` goes from a source node (`from`) to a destination node (`to`).\n\
Name nodes by their short key, like \"#12\" (shown as `key`/`ref` in every result; it never changes, even \
if the node is renamed). Start with search_nodes or get_graph to find keys, then use get_node / get_neighbours \
to explore. Changes appear live for everyone viewing the graph. Layout is automatic: new nodes are placed \
beside their destinations and neighbours make room, so you rarely need move_node. A batch (apply_batch) is \
atomic: if any op fails, none is applied. Deleting a node joins the links through it.";

fn node_ref_schema(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn tools() -> Vec<Value> {
    let mut all = tool_list();
    for t in all.iter_mut().filter(|t| t["name"] != "connect") {
        t["inputSchema"]["properties"]["graph"] = json!({ "type": "string", "description": "Which connected graph to use (its name from `connect`). Default: the most recently connected." });
    }
    all
}

fn tool_list() -> Vec<Value> {
    let node = |d: &str| node_ref_schema(d);
    let node_help = "Node reference: a key like \"#12\" (preferred), a node id, or \"$0\" for a node created earlier in the same batch.";
    vec![
        json!({
            "name": "connect",
            "description": "Open a graph with a key the user gave you (a bare key or a share link). Needed once before any other tool when the server was started without a key; call it again to switch graphs. The key decides what you may do: viewer reads, editor also changes. Never ask for or use an owner key unless the user insists. Several graphs can be open at once: give each a `name`, then pass that name as `graph` to the other tools (without `graph` they use the most recently connected one).",
            "inputSchema": { "type": "object", "properties": { "name": { "type": "string", "description": "A short name for this graph. Default: its title." }, "key": { "type": "string", "description": "The key (wp_editor_…, wp_viewer_… or wp_owner_…) or a link containing it." } }, "required": ["key"] }
        }),
        json!({
            "name": "get_graph",
            "description": "Summary of the graph: how many nodes and links it has, and how many `conflicts` (see get_conflicts).",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "get_full_graph",
            "description": "The whole graph as JSON: every node (key, title, note, position), every link (a link from source `from` to destination `to`) and the set-aside conflicts.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "search_nodes",
            "description": "List nodes, optionally filtered by a case-insensitive title substring (or a key like \"#12\"). Ordered by key. Returns each node's key, ref and title.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string", "description": "Text to find in titles, or a key like \"#12\". Omit to list everything." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Maximum nodes to return (default 50)." }
            } }
        }),
        json!({
            "name": "get_node",
            "description": "One node with its position and its direct links: `destinations` (the nodes it links to) and `sources` (the nodes that link to it).",
            "inputSchema": { "type": "object", "required": ["node"], "properties": { "node": node(node_help) } }
        }),
        json!({
            "name": "get_neighbours",
            "description": "The part of the graph around a node, up to `depth` links away (default 1, max 5, at most 200 nodes). Links are listed as {from, to}, from source to destination.",
            "inputSchema": { "type": "object", "required": ["node"], "properties": {
                "node": node(node_help),
                "direction": { "type": "string", "enum": ["destinations", "sources", "both"], "description": "destinations = what it links to (transitively); sources = what links to it. Default both. (The old names dependencies and dependants still work.)" },
                "depth": { "type": "integer", "minimum": 1, "maximum": 5 }
            } }
        }),
        json!({
            "name": "get_dot",
            "description": "The whole graph as Graphviz DOT text: one line per node (with its key) and one arrow per link. An arrow A -> B is a link from source A to destination B. The quickest way to see everything at once on a small graph.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "get_conflicts",
            "description": "Links the server set aside because two people's changes, made at the same time, together closed a cycle. They are hidden from every other result. Each is {from, to}, from source to destination. Resolve one with unlink_nodes (remove it) after deciding which link is wrong.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "create_node",
            "description": "Add a node. It is placed next to its first destination (or before its first source, or below everything), and neighbours make room. Returns its new key.",
            "inputSchema": { "type": "object", "required": ["title"], "properties": {
                "title": { "type": "string", "description": "Short title, at most 200 characters." },
                "destinations": { "type": "array", "items": { "type": "string" }, "description": "Nodes the new node links to (node references); alias depends_on." },
                "sources": { "type": "array", "items": { "type": "string" }, "description": "Existing nodes that should link to the new node (node references); alias dependants." },
                "node_type": { "type": "string", "description": "Name of a node type (see get_types). Default: the default type." }
            } }
        }),
        json!({
            "name": "rename_node",
            "description": "Change a node's title. Its key stays the same.",
            "inputSchema": { "type": "object", "required": ["node", "title"], "properties": { "node": node(node_help), "title": { "type": "string" } } }
        }),
        json!({
            "name": "delete_node",
            "description": "Delete a node. Chains through it are joined: deleting B from \"C -> B -> A\" leaves \"C -> A\". Its key is never reused.",
            "inputSchema": { "type": "object", "required": ["node"], "properties": { "node": node(node_help) } }
        }),
        json!({
            "name": "link_nodes",
            "description": "Link `from` (the source) to `to` (the destination). Refused if it would create a cycle or if from == to. Linking twice is harmless; linking an existing link with another `edge_type` changes its type.",
            "inputSchema": { "type": "object", "required": ["from", "to"], "properties": {
                "from": node(node_help), "to": node(node_help),
                "edge_type": { "type": "string", "description": "Name of an edge type (see get_types). Default: link (the old name depends_on still works)." }
            } }
        }),
        json!({
            "name": "unlink_nodes",
            "description": "Remove the link from `from` to `to`. Nodes keep their positions.",
            "inputSchema": { "type": "object", "required": ["from", "to"], "properties": { "from": node(node_help), "to": node(node_help) } }
        }),
        json!({
            "name": "move_node",
            "description": "Move a node so its top-left corner is at (x, y) in world units (nodes are 160 wide and 56 tall; about 24 units of space is kept between nodes). Nodes in the way are pushed aside. Rarely needed: placement is automatic.",
            "inputSchema": { "type": "object", "required": ["node", "x", "y"], "properties": {
                "node": node(node_help),
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "with_destinations": { "type": "boolean", "description": "Also move the nodes it links to, and what they link to (default false: just this node)." }
            } }
        }),
        json!({
            "name": "undo",
            "description": "Undo your own latest change (the last batch this server session sent: a write tool, or apply_batch). Other people's and agents' changes are never undone. Fails if there is nothing to undo. Edits typed directly into a note in the web UI are not in this history.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "redo",
            "description": "Redo the change you just undid. Fails if there is nothing to redo.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "get_types",
            "description": "The node types and edge types of this graph with their styles, and the canvas `background` colour. Read this before styling.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "set_node_type",
            "description": "Give a node a type (by name); \"default\" restores the default.",
            "inputSchema": { "type": "object", "required": ["node", "node_type"], "properties": { "node": node(node_help), "node_type": { "type": "string" } } }
        }),
        json!({
            "name": "upsert_node_type",
            "description": "Create a node type, or change the one with this `name`; only the fields you give change. Colours are #rrggbb. `bg` and `fg` (box fill and title colour) must be given together and need a contrast of at least 4.5:1. `border_width` is clamped to 0-10.",
            "inputSchema": { "type": "object", "required": ["name"], "properties": {
                "name": { "type": "string", "description": "Handle: lowercase a-z, 0-9, _ - ." },
                "new_name": { "type": "string", "description": "Rename the type." },
                "shape": { "type": "string", "enum": ["rect", "pill", "ellipse", "hexagon", "rhombus"] },
                "bg": { "type": "string" }, "fg": { "type": "string" }, "border": { "type": "string" },
                "border_width": { "type": "integer", "minimum": 0, "maximum": 10 },
                "corners": { "type": "string", "enum": ["round", "square"], "description": "Corner style of rect, hexagon and rhombus shapes." }
            } }
        }),
        json!({
            "name": "upsert_edge_type",
            "description": "Create an edge type, or change the one with this `name`; only the fields you give change. Colours are #rrggbb and are drawn on the canvas background (see get_types): `color` (the line) needs a contrast of at least 3:1 and `text_color` (the label) at least 4.5:1. `width` is clamped to 1-10. `start` is the end at the source, `end` at the destination.",
            "inputSchema": { "type": "object", "required": ["name"], "properties": {
                "name": { "type": "string" }, "new_name": { "type": "string" }, "label": { "type": "string" },
                "color": { "type": "string" }, "text_color": { "type": "string" },
                "width": { "type": "integer", "minimum": 1, "maximum": 10 },
                "line_style": { "type": "string", "enum": ["solid", "dashed", "dotted"] },
                "start": { "type": "string", "enum": ["none", "arrow", "dot", "bar"] },
                "end": { "type": "string", "enum": ["none", "arrow", "dot", "bar"] }
            } }
        }),
        json!({
            "name": "remove_node_type",
            "description": "Remove a node type; nodes that used it show the default style. The default type cannot be removed.",
            "inputSchema": { "type": "object", "required": ["name"], "properties": { "name": { "type": "string" } } }
        }),
        json!({
            "name": "remove_edge_type",
            "description": "Remove an edge type; links that used it show the default style. The default type cannot be removed.",
            "inputSchema": { "type": "object", "required": ["name"], "properties": { "name": { "type": "string" } } }
        }),
        json!({
            "name": "set_note",
            "description": "Replace a node's note. Every node has one note (Markdown allowed), shared by everyone and edited live in the margin of the web UI. Only the part that changed is edited, so someone typing elsewhere in the note is not overwritten. Send the whole new text; an empty string clears it. To add something without risking others' text, use append_note. get_node returns the current note.",
            "inputSchema": { "type": "object", "required": ["node", "body"], "properties": { "node": node(node_help), "body": { "type": "string", "description": "The whole note, at most 20000 characters." } } }
        }),
        json!({
            "name": "append_note",
            "description": "Add text to the end of a node's note, after a blank line if it already has text.",
            "inputSchema": { "type": "object", "required": ["node", "text"], "properties": { "node": node(node_help), "text": { "type": "string" } } }
        }),
        json!({
            "name": "apply_batch",
            "description": "Apply several operations atomically (all or none), at most 100. Each op is an object with an `op` field: create_node {title, destinations?, sources?}, rename_node {node, title}, delete_node {node}, link {from, to}, unlink {from, to}, move_node {node, x, y, with_destinations?}, set_note {node, body}, append_note {node, text}. A node created earlier in the batch can be referenced as \"$N\" where N is the index of its create_node op.",
            "inputSchema": { "type": "object", "required": ["ops"], "properties": {
                "ops": { "type": "array", "items": { "type": "object", "required": ["op"], "properties": { "op": { "type": "string", "enum": ["create_node", "rename_node", "delete_node", "link", "unlink", "move_node", "set_node_type", "upsert_node_type", "upsert_edge_type", "remove_node_type", "remove_edge_type", "set_note", "append_note"] } } } },
                "label": { "type": "string", "description": "Optional note on what this batch is for." }
            } }
        }),
    ]
}

/// Percent-encode everything but unreserved characters, so keys like `#12` are safe in a URL.
pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn str_arg<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args.get(name).and_then(Value::as_str).ok_or_else(|| format!("missing or non-string argument `{name}`"))
}

fn call_tool(name: &str, args: &Value, cfg: &Config, backend: &dyn Backend) -> Result<Value, String> {
    if name == "connect" {
        let key = extract_key(str_arg(args, "key")?).ok_or("that does not contain a Waypoint key (they start with wp_owner_, wp_editor_ or wp_viewer_)")?;
        return backend.connect(&key, args.get("name").and_then(Value::as_str));
    }
    let graph = backend.select(args.get("graph").and_then(Value::as_str))?.unwrap_or_else(|| cfg.graph.clone());
    if graph.is_empty() {
        return Err("not connected to a graph yet: ask the user for a Waypoint key (or a share link) and call `connect` with it".into());
    }
    let graph = encode(&graph);
    let base = format!("/api/graphs/{graph}");
    // Write tools are all one batch of one op.
    let one = |mut op: Value| {
        // Optional arguments the caller left out are left out of the op, not sent as null.
        op.as_object_mut().map(|o| o.retain(|_, v| !v.is_null()));
        backend.post(&format!("{base}/apply"), &json!({ "ops": [op] }))
    };
    let node = |a: &Value, n: &str| str_arg(a, n).map(str::to_owned);

    match name {
        "get_graph" => backend.get(&base),
        "get_full_graph" => backend.get(&format!("{base}/full")),
        "get_dot" => backend.get_text(&format!("{base}/dot")),
        "get_conflicts" => backend.get(&format!("{base}/conflicts")),
        "search_nodes" => {
            let mut path = format!("{base}/nodes?limit={}", args.get("limit").and_then(Value::as_u64).unwrap_or(50));
            if let Some(q) = args.get("query").and_then(Value::as_str) {
                path.push_str(&format!("&q={}", encode(q)));
            }
            backend.get(&path)
        }
        "get_node" => backend.get(&format!("{base}/nodes/{}", encode(str_arg(args, "node")?))),
        "get_neighbours" => backend.get(&format!(
            "{base}/nodes/{}/neighbours?direction={}&depth={}",
            encode(str_arg(args, "node")?),
            encode(args.get("direction").and_then(Value::as_str).unwrap_or("both")),
            args.get("depth").and_then(Value::as_u64).unwrap_or(1)
        )),
        "create_node" => one(json!({
            "op": "create_node",
            "title": str_arg(args, "title")?,
            "destinations": args.get("destinations").or(args.get("depends_on")).cloned().unwrap_or(json!([])),
            "sources": args.get("sources").or(args.get("dependants")).cloned().unwrap_or(json!([])),
            "node_type": args.get("node_type"),
        }))
        .map(|r| r["results"][0].clone()),
        "rename_node" => one(json!({ "op": "rename_node", "node": node(args, "node")?, "title": str_arg(args, "title")? }))
            .map(|r| r["results"][0].clone()),
        "delete_node" => one(json!({ "op": "delete_node", "node": node(args, "node")? })).map(|r| r["results"][0].clone()),
        "link_nodes" => one(json!({ "op": "link", "from": node(args, "from")?, "to": node(args, "to")?, "edge_type": args.get("edge_type") }))
            .map(|r| r["results"][0].clone()),
        "unlink_nodes" => one(json!({ "op": "unlink", "from": node(args, "from")?, "to": node(args, "to")? }))
            .map(|r| r["results"][0].clone()),
        "move_node" => one(json!({
            "op": "move_node",
            "node": node(args, "node")?,
            "x": args.get("x").and_then(Value::as_i64).ok_or("missing integer argument `x`")?,
            "y": args.get("y").and_then(Value::as_i64).ok_or("missing integer argument `y`")?,
            "with_destinations": args.get("with_destinations").or(args.get("with_dependencies")).or(args.get("with_subtree")).and_then(Value::as_bool).unwrap_or(false),
        }))
        .map(|r| r["results"][0].clone()),
        "undo" | "redo" => backend.post(&format!("{base}/{name}"), &json!({})),
        "get_types" => backend.get(&format!("{base}/types")),
        "set_node_type" => one(json!({ "op": "set_node_type", "node": node(args, "node")?, "node_type": str_arg(args, "node_type")? }))
            .map(|r| r["results"][0].clone()),
        "upsert_node_type" | "upsert_edge_type" | "remove_node_type" | "remove_edge_type" => {
            let mut op = args.clone();
            op["op"] = json!(name);
            one(op).map(|r| r["results"][0].clone())
        }
        "set_note" => one(json!({ "op": "set_note", "node": node(args, "node")?, "body": str_arg(args, "body")? }))
            .map(|r| r["results"][0].clone()),
        "append_note" => one(json!({ "op": "append_note", "node": node(args, "node")?, "text": str_arg(args, "text")? }))
            .map(|r| r["results"][0].clone()),
        "apply_batch" => {
            let ops = args.get("ops").filter(|o| o.is_array()).ok_or("missing array argument `ops`")?;
            backend.post(&format!("{base}/apply"), &json!({ "ops": ops, "label": args.get("label") }))
        }
        other => Err(format!("unknown tool `{other}`")),
    }
}

fn rpc_error(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Handle one JSON-RPC message. Returns the reply, or `None` for notifications.
pub fn handle(request: &Value, cfg: &Config, backend: &dyn Backend) -> Option<Value> {
    let id = request.get("id")?; // no id: a notification, which gets no reply
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Some(rpc_error(id, -32600, "invalid request: no method"));
    };
    let params = request.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = if SUPPORTED_VERSIONS.contains(&asked) { asked } else { SUPPORTED_VERSIONS[2] };
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "waypoint", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS,
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(rpc_error(id, -32602, "tools/call needs a tool `name`"));
            };
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            // A failing tool is a normal result flagged isError, so the model sees the message and can recover.
            match call_tool(name, &args, cfg, backend) {
                Ok(value) => {
                    // Text replies (DOT) go through as they are; everything else is pretty JSON.
                    let text = match &value {
                        Value::String(s) => s.clone(),
                        v => serde_json::to_string_pretty(v).unwrap_or_default(),
                    };
                    json!({ "content": [{ "type": "text", "text": text }], "isError": false })
                }
                Err(message) => json!({ "content": [{ "type": "text", "text": message }], "isError": true }),
            }
        }
        other => return Some(rpc_error(id, -32601, &format!("method not found: {other}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn keys_are_found_in_pasted_text() {
        assert_eq!(super::extract_key("wp_editor_ab12").as_deref(), Some("wp_editor_ab12"));
        assert_eq!(super::extract_key("https://x.net/#key=wp_viewer_9f&z=1").as_deref(), Some("wp_viewer_9f"));
        assert_eq!(super::extract_key("no key here"), None);
    }

    use super::*;
    use std::cell::RefCell;

    /// Records calls and replies with canned values.
    #[derive(Default)]
    struct Fake {
        calls: RefCell<Vec<String>>,
        fail_with: Option<String>,
    }

    impl Backend for Fake {
        fn get(&self, path: &str) -> Result<Value, String> {
            self.calls.borrow_mut().push(format!("GET {path}"));
            self.fail_with.clone().map_or(Ok(json!({ "ok": true })), Err)
        }
        fn get_text(&self, path: &str) -> Result<Value, String> {
            self.calls.borrow_mut().push(format!("GET {path}"));
            self.fail_with.clone().map_or(Ok(json!("digraph waypoint {}")), Err)
        }
        fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
            self.calls.borrow_mut().push(format!("POST {path} {body}"));
            self.fail_with.clone().map_or(Ok(json!({ "results": [{ "op": "x", "key": 7 }] })), Err)
        }
    }

    fn cfg() -> Config {
        Config { graph: "demo".into() }
    }

    fn call(fake: &Fake, name: &str, args: Value) -> Value {
        let req = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": name, "arguments": args } });
        handle(&req, &cfg(), fake).unwrap()["result"].clone()
    }

    #[test]
    fn initialize_negotiates_the_protocol_version() {
        let fake = Fake::default();
        let ask = |v: &str| handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": v } }), &cfg(), &fake).unwrap();
        assert_eq!(ask("2025-03-26")["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(ask("1999-01-01")["result"]["protocolVersion"], "2025-06-18", "unknown versions get our newest");
        let r = &ask("2025-06-18")["result"];
        assert_eq!(r["serverInfo"]["name"], "waypoint");
        assert!(r["capabilities"]["tools"].is_object());
        assert!(r["instructions"].as_str().unwrap().contains("destination"));
    }

    #[test]
    fn notifications_get_no_reply_and_unknown_methods_get_an_error() {
        let fake = Fake::default();
        assert!(handle(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }), &cfg(), &fake).is_none());
        let r = handle(&json!({ "jsonrpc": "2.0", "id": 5, "method": "nope" }), &cfg(), &fake).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        assert_eq!(r["id"], 5);
        assert!(handle(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }), &cfg(), &fake).unwrap()["result"].is_object());
    }

    #[test]
    fn every_tool_has_a_description_and_an_object_schema_and_unique_name() {
        let list = tools();
        let mut names: Vec<_> = list.iter().map(|t| t["name"].as_str().unwrap().to_owned()).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(names.len(), before, "tool names must be unique");
        for t in &list {
            assert!(t["description"].as_str().unwrap().len() > 20, "{} needs a real description", t["name"]);
            assert_eq!(t["inputSchema"]["type"], "object");
            // Every required argument is declared as a property.
            for req in t["inputSchema"]["required"].as_array().into_iter().flatten() {
                assert!(t["inputSchema"]["properties"][req.as_str().unwrap()].is_object(), "{}: {req}", t["name"]);
            }
        }
        assert_eq!(names.len(), 25);
    }

    #[test]
    fn keys_are_percent_encoded_in_urls() {
        assert_eq!(encode("#12"), "%2312");
        assert_eq!(encode("a b/c"), "a%20b%2Fc");
        assert_eq!(encode("00000000-0000-7000-8000-000000001001"), "00000000-0000-7000-8000-000000001001");
        let fake = Fake::default();
        call(&fake, "get_node", json!({ "node": "#12" }));
        assert_eq!(fake.calls.borrow()[0], "GET /api/graphs/demo/nodes/%2312");
    }

    #[test]
    fn write_tools_become_one_op_batches() {
        let fake = Fake::default();
        let r = call(&fake, "link_nodes", json!({ "from": "#3", "to": "#1" }));
        assert_eq!(r["isError"], false);
        // Compared as JSON: key order depends on which serde_json features the build unified.
        let call = fake.calls.borrow()[0].clone();
        let (head, body) = call.split_once(" {").expect("a body");
        assert_eq!(head, "POST /api/graphs/demo/apply");
        let body: Value = serde_json::from_str(&format!("{{{body}")).unwrap();
        assert_eq!(body, json!({ "ops": [{ "op": "link", "from": "#3", "to": "#1" }] }));
        // Only the op's result comes back, not the batch wrapper.
        assert!(r["content"][0]["text"].as_str().unwrap().contains("\"key\": 7"));
    }

    #[test]
    fn create_node_sends_destinations_and_defaults_the_rest() {
        let fake = Fake::default();
        call(&fake, "create_node", json!({ "title": "Write docs", "destinations": ["#2"] }));
        let c = fake.calls.borrow()[0].clone();
        assert!(c.starts_with("POST /api/graphs/demo/apply "), "{c}");
        assert!(c.contains(r##""destinations":["#2"]"##) && c.contains(r#""sources":[]"#) && c.contains("Write docs"), "{c}");
    }

    #[test]
    fn reads_build_the_right_urls() {
        let fake = Fake::default();
        call(&fake, "search_nodes", json!({ "query": "write docs", "limit": 5 }));
        call(&fake, "get_neighbours", json!({ "node": "#1", "direction": "sources", "depth": 3 }));
        call(&fake, "get_graph", json!({}));
        call(&fake, "get_conflicts", json!({}));
        let calls = fake.calls.borrow();
        assert_eq!(calls[0], "GET /api/graphs/demo/nodes?limit=5&q=write%20docs");
        assert_eq!(calls[1], "GET /api/graphs/demo/nodes/%231/neighbours?direction=sources&depth=3");
        assert_eq!(calls[2], "GET /api/graphs/demo");
        assert_eq!(calls[3], "GET /api/graphs/demo/conflicts");
    }

    #[test]
    fn failures_are_results_the_model_can_read_not_protocol_errors() {
        let fake = Fake { fail_with: Some("link would create a cycle".into()), ..Fake::default() };
        let r = call(&fake, "link_nodes", json!({ "from": "#1", "to": "#2" }));
        assert_eq!(r["isError"], true);
        assert_eq!(r["content"][0]["text"], "link would create a cycle");

        let none = Fake::default();
        assert_eq!(call(&none, "link_nodes", json!({ "from": "#1" }))["isError"], true, "missing argument");
        assert!(call(&none, "link_nodes", json!({ "from": "#1" }))["content"][0]["text"].as_str().unwrap().contains("`to`"));
        assert_eq!(call(&none, "no_such_tool", json!({}))["isError"], true);
        assert!(none.calls.borrow().is_empty(), "bad calls never reach the server");
    }

    #[test]
    fn apply_batch_passes_ops_through() {
        let fake = Fake::default();
        let ops = json!([{ "op": "create_node", "title": "a" }, { "op": "link", "from": "$0", "to": "#1" }]);
        call(&fake, "apply_batch", json!({ "ops": ops, "label": "seed" }));
        let c = fake.calls.borrow()[0].clone();
        assert!(c.contains(r#""$0""#) && c.contains(r#""label":"seed""#), "{c}");
        assert_eq!(call(&Fake::default(), "apply_batch", json!({ "ops": "nope" }))["isError"], true);
    }
}
