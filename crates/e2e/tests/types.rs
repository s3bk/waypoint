mod common;

use common::{Stack, until};
use serde_json::json;
use thirtyfour::{By, Key};

#[tokio::test(flavor = "multi_thread")]
async fn types_are_styled_through_the_api_and_the_panel() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let apply = |ops: serde_json::Value| stack.call(&keys, &keys.editor, "POST", "/apply", Some(json!({ "ops": ops })));
    let seeded = apply(json!([
        { "op": "upsert_node_type", "name": "risk", "bg": "#7a1f1f", "fg": "#ffffff", "shape": "hexagon" },
        { "op": "create_node", "title": "outage", "node_type": "risk" },
    ]));
    assert_eq!(seeded.0, 200, "{}", seeded.1);

    // Contrast is enforced by the server, and the reason comes back.
    let bad = apply(json!([{ "op": "upsert_node_type", "name": "dim", "bg": "#777777", "fg": "#808080" }]));
    assert_eq!(bad.0, 422);
    assert!(bad.1["error"].as_str().unwrap().contains("contrast"), "{}", bad.1);

    let page = stack.page(&keys.owner).await;
    until("the node", || async { page.id_titled("outage").await.is_some() }).await;

    // The owner sets the canvas colour; the page and the edge types follow.
    let set = stack.call(&keys, &keys.owner, "PUT", "/background", Some(json!({ "color": "#f4f1ea" })));
    assert_eq!(set.0, 200, "{}", set.1);
    assert_eq!(set.1["recoloured_edge_types"], json!(["link"]));
    until("the canvas colour", || async {
        page.js("return document.documentElement.style.background").await.as_str().is_some_and(|c| c.contains("244, 241, 234") || c.contains("f4f1ea"))
    })
    .await;
    assert_eq!(stack.call(&keys, &keys.editor, "PUT", "/background", Some(json!({ "color": "#000000" }))).0, 403);

    // Select the node, so that a leaked Delete would remove it.
    let id = page.id_titled("outage").await.unwrap();
    let (cx, cy) = page.rect(&id).await.center();
    page.click(cx, cy).await;
    until("selected", || async { page.selected_node().await.as_deref() == Some(id.as_str()) }).await;

    // The panel lists the types, and a new one made in it reaches the server.
    page.driver.find(By::Id("types-button")).await.unwrap().click().await.unwrap();
    until("risk in the panel", || async {
        page.js("return document.getElementById('types-panel').textContent").await.as_str().is_some_and(|t| t.contains("risk"))
    })
    .await;
    page.js("[...document.querySelectorAll('#types-panel button')].find(b => b.textContent.includes('new edge type')).click()").await;
    until("the form", || async { page.js("return !!document.querySelector('#types-panel .form')").await == json!(true) }).await;
    let name = page.driver.find(By::Css("#types-panel .form input")).await.unwrap();
    name.send_keys("blocks").await.unwrap();
    // Keys typed in the panel do not reach the graph: Delete, Backspace and Ctrl+Z leave the node alone.
    name.send_keys(Key::Delete).await.unwrap();
    name.send_keys(Key::Backspace).await.unwrap(); // removes the last letter; retyped below
    name.send_keys(Key::Control + "z").await.unwrap(); // undoes typing in the field, as it should, not the graph
    name.send_keys(Key::Control + "a").await.unwrap();
    name.send_keys("blocks").await.unwrap();
    assert_eq!(stack.node_count(&keys), 1);
    assert_eq!(page.id_titled("outage").await, Some(id.clone()));
    page.js("[...document.querySelectorAll('#types-panel button')].find(b => b.textContent === 'Save').click()").await;
    until("the edge type on the server", || async {
        let t = stack.call(&keys, &keys.viewer, "GET", "/types", None).1;
        t["edge_types"].as_array().unwrap().iter().any(|e| e["name"] == "blocks")
    })
    .await;
}
