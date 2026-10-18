mod common;

use common::{Stack, until};
use serde_json::json;

/// A client that bypasses the cycle check (as a concurrent one effectively does)
/// gets its edge flagged by the server, and the browser keeps showing it.
#[tokio::test(flavor = "multi_thread")]
async fn an_edge_closing_a_cycle_is_flagged_and_kept() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let page = stack.page(&keys.editor).await;
    let batch = json!({ "ops": [{ "op": "create_node", "title": "a" }, { "op": "create_node", "title": "b", "depends_on": ["$0"] }] });
    assert_eq!(stack.call(&keys, &keys.editor, "POST", "/apply", Some(batch)).0, 200);
    until("both nodes", || async { page.edge_count().await == 1 }).await;

    let a = page.id_titled("a").await.unwrap();
    let b = page.id_titled("b").await.unwrap();
    page.js(&format!("window.__waypoint.graph.createEdge('{a}', '{b}', '00000000-0000-7000-8000-000000000001')")).await;

    until("the server to flag the newer edge", || async {
        page.js("return window.__waypoint.graph.edges().filter(e => e.conflicted).length").await == json!(1)
    })
    .await;
    assert_eq!(page.edge_count().await, 2, "the edge is kept, not dropped");
}
