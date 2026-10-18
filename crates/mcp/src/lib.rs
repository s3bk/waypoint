//! An MCP (Model Context Protocol) server that lets an AI agent read and edit a
//! Waypoint graph. It speaks JSON-RPC over stdio and forwards each tool call
//! to the Waypoint server's HTTP API, so edits reach everyone connected to the
//! graph live, exactly like a person's.
//!
//! Nodes are named by *reference*: a short key like `"#12"` (assigned by the
//! server, stable across renames), an id, or `"$0"` for a node created earlier
//! in the same batch. Links are identified by the ordered pair of their ends:
//! "from depends on to".

use serde_json::{Value, json};

/// The versions of the protocol this server can speak, newest last.
const SUPPORTED_VERSIONS: [&str; 3] = ["2024-11-05", "2025-03-26", "2025-06-18"];

/// Where tool calls go. The real one is HTTP; tests use a recording fake.
pub trait Backend {
    fn get(&self, path_and_query: &str) -> Result<Value, String>;
    fn post(&self, path: &str, body: &Value) -> Result<Value, String>;
    /// GET a plain-text reply (such as DOT); it comes back as a JSON string.
    fn get_text(&self, path: &str) -> Result<Value, String>;
}

pub struct Config {
    /// The graph the key opens, identified by its viewer key (the server tells us).
    pub graph: String,
}

const INSTRUCTIONS: &str = "\
Waypoint holds a graph of project work items (nodes) joined by dependency links.\n\
A link `from -> to` means \"from depends on to\": `to` must be done first.\n\
Name nodes by their short key, like \"#12\" (shown as `key`/`ref` in every result; it never changes, even \
if the node is renamed). Start with search_nodes or get_graph to find keys, then use get_node / get_neighbours \
to explore. Changes appear live for everyone viewing the graph. Layout is automatic: new nodes are placed \
beside what they depend on and neighbours make room, so you rarely need move_node. A batch (apply_batch) is \
atomic: if any op fails, none is applied. Deleting a node joins the links through it.";

fn node_ref_schema(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn tools() -> Vec<Value> {
    let node = |d: &str| node_ref_schema(d);
    let node_help = "Node reference: a key like \"#12\" (preferred), a node id, or \"$0\" for a node created earlier in the same batch.";
    vec![
        json!({
            "name": "get_graph",
            "description": "Summary of the graph: how many nodes and links it has, and how many `conflicts` (see get_conflicts).",
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
            "description": "One node with its position and its direct links: `depends_on` (what it needs first) and `dependants` (what needs it).",
            "inputSchema": { "type": "object", "required": ["node"], "properties": { "node": node(node_help) } }
        }),
        json!({
            "name": "get_neighbours",
            "description": "The part of the graph around a node, up to `depth` links away (default 1, max 5, at most 200 nodes). Links are listed as {from, to}, meaning \"from depends on to\".",
            "inputSchema": { "type": "object", "required": ["node"], "properties": {
                "node": node(node_help),
                "direction": { "type": "string", "enum": ["dependencies", "dependants", "both"], "description": "dependencies = what it depends on (transitively); dependants = what depends on it. Default both." },
                "depth": { "type": "integer", "minimum": 1, "maximum": 5 }
            } }
        }),
        json!({
            "name": "get_dot",
            "description": "The whole graph as Graphviz DOT text: one line per node (with its key) and one arrow per link. An arrow A -> B means \"A depends on B\". The quickest way to see everything at once on a small graph.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "get_conflicts",
            "description": "Links the server set aside because two people's changes, made at the same time, together closed a dependency cycle. They are hidden from every other result. Each is {from, to}, meaning \"from depends on to\". Resolve one with unlink_nodes (remove it) after deciding which dependency is wrong.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "create_node",
            "description": "Add a node. It is placed next to the first node it depends on (or before the first of its dependants, or below everything), and neighbours make room. Returns its new key.",
            "inputSchema": { "type": "object", "required": ["title"], "properties": {
                "title": { "type": "string", "description": "Short title, at most 200 characters." },
                "depends_on": { "type": "array", "items": { "type": "string" }, "description": "Nodes the new node depends on (node references)." },
                "dependants": { "type": "array", "items": { "type": "string" }, "description": "Existing nodes that should depend on the new node (node references)." }
            } }
        }),
        json!({
            "name": "rename_node",
            "description": "Change a node's title. Its key stays the same.",
            "inputSchema": { "type": "object", "required": ["node", "title"], "properties": { "node": node(node_help), "title": { "type": "string" } } }
        }),
        json!({
            "name": "delete_node",
            "description": "Delete a node. Chains through it are joined: deleting B from \"C depends on B depends on A\" leaves \"C depends on A\". Its key is never reused.",
            "inputSchema": { "type": "object", "required": ["node"], "properties": { "node": node(node_help) } }
        }),
        json!({
            "name": "link_nodes",
            "description": "Make `from` depend on `to` (`to` must be done first). Refused if it would create a dependency cycle or if from == to. Linking twice is harmless.",
            "inputSchema": { "type": "object", "required": ["from", "to"], "properties": { "from": node(node_help), "to": node(node_help) } }
        }),
        json!({
            "name": "unlink_nodes",
            "description": "Remove the link saying `from` depends on `to`. Nodes keep their positions.",
            "inputSchema": { "type": "object", "required": ["from", "to"], "properties": { "from": node(node_help), "to": node(node_help) } }
        }),
        json!({
            "name": "move_node",
            "description": "Move a node so its top-left corner is at (x, y) in world units (nodes are 160 wide and 56 tall; about 24 units of space is kept between nodes). Nodes in the way are pushed aside. Rarely needed: placement is automatic.",
            "inputSchema": { "type": "object", "required": ["node", "x", "y"], "properties": {
                "node": node(node_help),
                "x": { "type": "integer" },
                "y": { "type": "integer" },
                "with_subtree": { "type": "boolean", "description": "Also move the nodes anchored beneath it (default false: just this node)." }
            } }
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
            "description": "Apply several operations atomically (all or none), at most 100. Each op is an object with an `op` field: create_node {title, depends_on?, dependants?}, rename_node {node, title}, delete_node {node}, link {from, to}, unlink {from, to}, move_node {node, x, y, with_subtree?}, set_note {node, body}, append_note {node, text}. A node created earlier in the batch can be referenced as \"$N\" where N is the index of its create_node op.",
            "inputSchema": { "type": "object", "required": ["ops"], "properties": {
                "ops": { "type": "array", "items": { "type": "object", "required": ["op"], "properties": { "op": { "type": "string", "enum": ["create_node", "rename_node", "delete_node", "link", "unlink", "move_node", "set_note", "append_note"] } } } },
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
    let graph = encode(&cfg.graph);
    let base = format!("/api/graphs/{graph}");
    // Write tools are all one batch of one op.
    let one = |op: Value| backend.post(&format!("{base}/apply"), &json!({ "ops": [op] }));
    let node = |a: &Value, n: &str| str_arg(a, n).map(str::to_owned);

    match name {
        "get_graph" => backend.get(&base),
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
            "depends_on": args.get("depends_on").cloned().unwrap_or(json!([])),
            "dependants": args.get("dependants").cloned().unwrap_or(json!([])),
        }))
        .map(|r| r["results"][0].clone()),
        "rename_node" => one(json!({ "op": "rename_node", "node": node(args, "node")?, "title": str_arg(args, "title")? }))
            .map(|r| r["results"][0].clone()),
        "delete_node" => one(json!({ "op": "delete_node", "node": node(args, "node")? })).map(|r| r["results"][0].clone()),
        "link_nodes" => one(json!({ "op": "link", "from": node(args, "from")?, "to": node(args, "to")? }))
            .map(|r| r["results"][0].clone()),
        "unlink_nodes" => one(json!({ "op": "unlink", "from": node(args, "from")?, "to": node(args, "to")? }))
            .map(|r| r["results"][0].clone()),
        "move_node" => one(json!({
            "op": "move_node",
            "node": node(args, "node")?,
            "x": args.get("x").and_then(Value::as_i64).ok_or("missing integer argument `x`")?,
            "y": args.get("y").and_then(Value::as_i64).ok_or("missing integer argument `y`")?,
            "with_subtree": args.get("with_subtree").and_then(Value::as_bool).unwrap_or(false),
        }))
        .map(|r| r["results"][0].clone()),
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
        assert!(r["instructions"].as_str().unwrap().contains("depends on"));
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
        assert_eq!(names.len(), 15);
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
    fn create_node_sends_dependencies_and_defaults_the_rest() {
        let fake = Fake::default();
        call(&fake, "create_node", json!({ "title": "Write docs", "depends_on": ["#2"] }));
        let c = fake.calls.borrow()[0].clone();
        assert!(c.starts_with("POST /api/graphs/demo/apply "), "{c}");
        assert!(c.contains(r##""depends_on":["#2"]"##) && c.contains(r#""dependants":[]"#) && c.contains("Write docs"), "{c}");
    }

    #[test]
    fn reads_build_the_right_urls() {
        let fake = Fake::default();
        call(&fake, "search_nodes", json!({ "query": "write docs", "limit": 5 }));
        call(&fake, "get_neighbours", json!({ "node": "#1", "direction": "dependants", "depth": 3 }));
        call(&fake, "get_graph", json!({}));
        call(&fake, "get_conflicts", json!({}));
        let calls = fake.calls.borrow();
        assert_eq!(calls[0], "GET /api/graphs/demo/nodes?limit=5&q=write%20docs");
        assert_eq!(calls[1], "GET /api/graphs/demo/nodes/%231/neighbours?direction=dependants&depth=3");
        assert_eq!(calls[2], "GET /api/graphs/demo");
        assert_eq!(calls[3], "GET /api/graphs/demo/conflicts");
    }

    #[test]
    fn failures_are_results_the_model_can_read_not_protocol_errors() {
        let fake = Fake { fail_with: Some("link would create a dependency cycle".into()), ..Fake::default() };
        let r = call(&fake, "link_nodes", json!({ "from": "#1", "to": "#2" }));
        assert_eq!(r["isError"], true);
        assert_eq!(r["content"][0]["text"], "link would create a dependency cycle");

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
