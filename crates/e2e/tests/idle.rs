mod common;

use common::{Stack, until};

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_page_gets_no_document_updates() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let page = stack.page(&keys.editor).await;
    page.dblclick(300, 300).await;
    page.type_title("alpha").await;
    until("alpha on the server", || async { stack.node_count(&keys) == 1 }).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;

    page.js("const w = window.__waypoint; w.counts = { updates: 0, awareness: 0 }; w.graph.doc.on('update', () => w.counts.updates++); w.onAwareness?.(() => w.counts.awareness++);").await;
    tokio::time::sleep(std::time::Duration::from_secs(8)).await;
    let counts = page.js("return window.__waypoint.counts").await;
    assert_eq!(counts["updates"], 0, "document updates while idle: {counts}");
}
