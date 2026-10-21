mod common;

use common::{Stack, until};
use serde_json::{Value, json};
use thirtyfour::By;

fn positions(stack: &Stack, keys: &common::Keys) -> Value {
    let full = stack.call(keys, &keys.viewer, "GET", "/full", None).1;
    full["nodes"].as_array().unwrap().iter().map(|n| (n["title"].as_str().unwrap().to_owned(), n["position"].clone())).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_layout_is_previewed_locally_and_only_sent_on_accept() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let made = stack.call(
        &keys,
        &keys.editor,
        "POST",
        "/apply",
        Some(json!({ "ops": [
            { "op": "create_node", "title": "a", "x": 0, "y": 0 },
            { "op": "create_node", "title": "b", "destinations": ["$0"], "x": 0, "y": 300 },
            { "op": "create_node", "title": "c", "destinations": ["$1"], "x": 0, "y": 600 },
        ] })),
    );
    assert_eq!(made.0, 200, "{}", made.1);
    let page = stack.page(&keys.editor).await;
    until("the nodes", || async { page.node_ids().await.len() == 3 }).await;
    let (a, c) = (page.id_titled("a").await.unwrap(), page.id_titled("c").await.unwrap());
    let before = positions(&stack, &keys);
    let (a0, c0) = (page.rect(&a).await, page.rect(&c).await);

    // The layers layout puts the chain in a row. It shows on the canvas; the server still has the old places.
    page.open_tool("layout-button").await;
    page.driver.find(By::Id("layout-layered")).await.unwrap().click().await.unwrap();
    until("the preview", || async { page.driver.find(By::Id("layout-accept")).await.is_ok() }).await;
    assert!(page.hint().await.contains("Previewing"));
    assert_eq!(positions(&stack, &keys), before, "a preview sends nothing");
    // Editing is off during a preview: a double-click on empty canvas adds no node.
    page.dblclick(700, 600).await;
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    assert_eq!(stack.node_count(&keys), 3);

    // Cancel goes back: same screen places, nothing sent.
    page.driver.find(By::Id("layout-discard")).await.unwrap().click().await.unwrap();
    until("the old layout", || async { let r = page.rect(&c).await; (r.x, r.y) == (c0.x, c0.y) }).await;
    assert_eq!((page.rect(&a).await.x, page.rect(&a).await.y), (a0.x, a0.y));

    // Accept writes it, as one undoable batch.
    page.driver.find(By::Id("layout-layered")).await.unwrap().click().await.unwrap();
    until("the preview again", || async { page.driver.find(By::Id("layout-accept")).await.is_ok() }).await;
    page.driver.find(By::Id("layout-accept")).await.unwrap().click().await.unwrap();
    until("the layout on the server", || async { positions(&stack, &keys) != before }).await;
    let after = positions(&stack, &keys);
    assert_eq!(after["a"]["y"], after["c"]["y"], "the chain is in one row: {after}");
    assert_eq!(stack.node_count(&keys), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_one_tool_is_open_at_a_time() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let page = stack.page(&keys.editor).await;
    let display = |panel: &str| format!("return getComputedStyle(document.getElementById('{panel}')).display");

    page.open_tool("layout-button").await;
    assert_eq!(page.js(&display("layout-panel")).await, json!("block"));
    page.open_tool("types-button").await;
    assert_eq!(page.js(&display("types-panel")).await, json!("block"));
    assert_eq!(page.js(&display("layout-panel")).await, json!("none"), "opening types closed layout");
    page.open_tool("share-button").await;
    assert_eq!(page.js(&display("share-panel")).await, json!("block"));
    assert_eq!(page.js(&display("types-panel")).await, json!("none"));
    // The burger closes whatever is open and shows the menu again.
    page.driver.find(By::Id("menu-button")).await.unwrap().click().await.unwrap();
    assert_eq!(page.js(&display("share-panel")).await, json!("none"));
    assert_eq!(page.js(&display("menu")).await, json!("flex"));
}
