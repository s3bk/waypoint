mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use common::{Stack, until, workspace};
use serde_json::{Value, json};

/// The real `waypoint-mcp` binary, spoken to over stdio.
struct Mcp {
    child: Child,
    stdin: ChildStdin,
    out: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(stack: &Stack, key: &str) -> Mcp {
        let mut child = Command::new(workspace().join("target/debug/waypoint-mcp"))
            .env("WAYPOINT_URL", &stack.api)
            .env("WAYPOINT_KEY", key)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("waypoint-mcp");
        let stdin = child.stdin.take().unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        Mcp { child, stdin, out, next: 0 }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let request = json!({ "jsonrpc": "2.0", "id": self.next, "method": method, "params": params });
        writeln!(self.stdin, "{request}").unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        serde_json::from_str(&line).expect("json-rpc reply")
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.rpc("tools/call", json!({ "name": name, "arguments": args }))
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn agents_edit_while_a_browser_watches() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let page = stack.page(&keys.viewer).await;

    // HTTP: a batch is atomic and `$N` names a node made earlier in it.
    let batch = json!({ "ops": [{ "op": "create_node", "title": "root" }, { "op": "create_node", "title": "child", "destinations": ["$0"] }] });
    assert_eq!(stack.call(&keys, &keys.editor, "POST", "/apply", Some(batch)).0, 200);
    until("the browser to show the batch", || async { page.titles().await == ["child", "root"] }).await;
    assert_eq!(page.edge_count().await, 1);

    let bad = json!({ "ops": [{ "op": "create_node", "title": "ghost" }, { "op": "link", "from": "#1", "to": "#999" }] });
    let (status, body) = stack.call(&keys, &keys.editor, "POST", "/apply", Some(bad));
    assert_eq!((status, &body["op_index"]), (422, &json!(1)), "{body}");
    assert_eq!(stack.node_count(&keys), 2, "a failing batch changes nothing");
    let cycle = json!({ "ops": [{ "op": "link", "from": "#1", "to": "#2" }] });
    assert_eq!(stack.call(&keys, &keys.editor, "POST", "/apply", Some(cycle)).0, 422);

    // MCP over stdio.
    let mut mcp = Mcp::start(&stack, &keys.editor);
    assert!(mcp.rpc("initialize", json!({ "protocolVersion": "2024-11-05" }))["result"].is_object());
    assert_eq!(mcp.rpc("tools/list", json!({}))["result"]["tools"].as_array().unwrap().len(), 27);
    let made = mcp.tool("create_node", json!({ "title": "layout", "destinations": ["#1"] }));
    assert_ne!(made["result"]["isError"], true, "{made}");
    until("the browser to show the MCP node", || async { page.titles().await.contains(&"layout".to_owned()) }).await;
    assert_eq!(page.edge_count().await, 2);
    mcp.tool("rename_node", json!({ "node": "#3", "title": "layout engine" }));
    until("the rename", || async { page.titles().await.contains(&"layout engine".to_owned()) }).await;
    let del = mcp.tool("delete_node", json!({ "node": "#3" }));
    assert_ne!(del["result"]["isError"], true, "{del}");
    until("the delete", || async { !page.titles().await.contains(&"layout engine".to_owned()) }).await;

    let mut viewer = Mcp::start(&stack, &keys.viewer);
    let refused = viewer.tool("create_node", json!({ "title": "nope" }));
    assert_eq!(refused["result"]["isError"], true, "a viewer key cannot write: {refused}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_without_a_key_connects_with_one_the_user_gives() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let mut mcp = Mcp::start(&stack, "");
    let before = mcp.tool("get_graph", json!({}));
    assert_eq!(before["result"]["isError"], true, "no key yet: {before}");
    assert!(before["result"]["content"][0]["text"].as_str().unwrap().contains("connect"));
    let bad = mcp.tool("connect", json!({ "key": "wp_editor_nope" }));
    assert_eq!(bad["result"]["isError"], true, "{bad}");
    // A share link works as well as a bare key.
    let link = format!("{}/#key={}", stack.api, keys.editor);
    let ok = mcp.tool("connect", json!({ "key": link, "name": "first" }));
    assert_ne!(ok["result"]["isError"], true, "{ok}");
    let made = mcp.tool("create_node", json!({ "title": "after connect" }));
    assert_ne!(made["result"]["isError"], true, "{made}");
    assert_eq!(stack.node_count(&keys), 1);

    // A second graph at the same time: `graph` picks between them, and the latest is the default.
    let other = stack.new_graph();
    let two = mcp.tool("connect", json!({ "key": other.editor, "name": "other" }));
    assert_ne!(two["result"]["isError"], true, "{two}");
    mcp.tool("create_node", json!({ "title": "in other" }));
    assert_eq!(stack.node_count(&other), 1);
    mcp.tool("create_node", json!({ "title": "back in first", "graph": "first" }));
    assert_eq!((stack.node_count(&keys), stack.node_count(&other)), (2, 1));
    // An agent can make a graph of its own; it is opened at once and carries its title.
    let fresh = mcp.tool("create_graph", json!({ "title": "made by agent", "name": "mine" }));
    assert_ne!(fresh["result"]["isError"], true, "{fresh}");
    let text = fresh["result"]["content"][0]["text"].as_str().unwrap().to_owned();
    assert!(text.contains("made by agent") && text.contains("wp_editor_"), "{text}");
    let node = mcp.tool("create_node", json!({ "title": "in mine" }));
    assert_ne!(node["result"]["isError"], true, "{node}");
    let first = mcp.tool("get_graph", json!({ "graph": "nope" }));
    assert_eq!(first["result"]["isError"], true, "{first}");
    let names = first["result"]["content"][0]["text"].as_str().unwrap().to_owned();
    assert!(names.contains("other"), "{names}");
}
