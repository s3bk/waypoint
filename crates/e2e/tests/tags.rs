mod common;

use common::{Stack, until};
use serde_json::json;

/// Tags are defined in the registry and put on nodes and links by an agent; the browser sees them.
#[tokio::test(flavor = "multi_thread")]
async fn tags_reach_the_browser() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let page = stack.page(&keys.editor).await;
    let batch = json!({ "ops": [
        { "op": "create_node", "title": "a" },
        { "op": "create_node", "title": "b", "destinations": ["$0"] },
        { "op": "upsert_tag", "name": "todo", "glyph": "◻", "color": "#e6b450" },
        { "op": "upsert_tag", "name": "done", "glyph": "✅" },
        { "op": "set_tags", "node": "$0", "add": ["todo", "done"] },
        { "op": "set_tags", "from": "$1", "to": "$0", "add": ["todo"] },
    ] });
    let (status, body) = stack.call(&keys, &keys.editor, "POST", "/apply", Some(batch));
    assert_eq!(status, 200, "{body}");
    until("the registry in the browser", || async {
        page.js("return window.__waypoint.graph.registry().tags.length").await == json!(2)
    })
    .await;
    let a = page.id_titled("a").await.unwrap();
    let b = page.id_titled("b").await.unwrap();
    let names = |expr: String| {
        let page = &page;
        async move { page.js(&format!("return {expr}.map(t => t.name).join(',')")).await }
    };
    until("tags on the node", || async { names(format!("window.__waypoint.graph.registry().nodeTags('{a}')")).await == json!("todo,done") }).await;
    assert_eq!(names(format!("window.__waypoint.graph.registry().edgeTags('{b}', '{a}')")).await, json!("todo"));
    assert_eq!(names(format!("window.__waypoint.graph.registry().nodeTags('{b}')")).await, json!(""));

    let node = stack.call(&keys, &keys.viewer, "GET", "/nodes/%231", None).1;
    assert_eq!(node["tags"], json!(["todo", "done"]));
}
