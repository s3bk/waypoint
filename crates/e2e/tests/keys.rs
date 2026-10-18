mod common;

use common::{Stack, until};
use serde_json::json;
use thirtyfour::By;

async fn share_rows(page: &common::Page) -> Vec<String> {
    page.js("return [...document.querySelectorAll('#share-panel div b')].map(b => b.textContent)").await.as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_key_opens_the_graph_with_its_role() {
    let stack = Stack::start();
    let keys = stack.new_graph();

    let editor = stack.page(&keys.editor).await;
    assert!(editor.role_label().await.starts_with("editor"));
    editor.dblclick(500, 300).await;
    editor.type_title("seed").await;
    until("seed on the server", || async { stack.node_count(&keys) == 1 }).await;

    let viewer = stack.page(&keys.viewer).await;
    assert!(viewer.role_label().await.starts_with("viewer"));
    until("the viewer to see the node live", || async { viewer.node_ids().await.len() == 1 }).await;
    assert!(viewer.hint().await.contains("Read-only"));
    viewer.dblclick(700, 600).await;
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert_eq!(stack.node_count(&keys), 1, "a viewer cannot add a node");
    let write = json!({ "ops": [{ "op": "create_node", "title": "x" }] });
    assert_eq!(stack.call(&keys, &keys.viewer, "POST", "/apply", Some(write)).0, 403);
    assert_eq!(stack.call(&keys, &format!("wp_editor_{}", "0".repeat(64)), "GET", "/nodes", None).0, 401);

    // The Share panel lists what each key can hand out.
    editor.driver.find(By::Id("share-button")).await.unwrap().click().await.unwrap();
    assert_eq!(share_rows(&editor).await, ["editor", "viewer"]);
    assert_eq!(share_rows(&viewer).await, ["viewer"]);
    let owner = stack.page(&keys.owner).await;
    assert_eq!(share_rows(&owner).await, ["owner", "editor", "viewer"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn new_graph_button_makes_a_graph_with_a_new_owner_key() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let owner = stack.page(&keys.owner).await;
    owner.driver.find(By::Id("share-button")).await.unwrap().click().await.unwrap();
    owner.driver.find(By::Id("new-graph-button")).await.unwrap().click().await.unwrap();
    // It asks for a name first, then shows the keys once and opens the new graph in a new tab.
    until("the name prompt", || async { owner.driver.get_alert_text().await.is_ok() }).await;
    owner.driver.send_alert_text("Plan B").await.unwrap();
    owner.driver.accept_alert().await.unwrap();
    until("the keys alert", || async { owner.driver.get_alert_text().await.is_ok() }).await;
    let text = owner.driver.get_alert_text().await.unwrap();
    assert!(text.contains("wp_owner_") && text.contains("wp_editor_") && text.contains("wp_viewer_"), "{text}");
    owner.driver.accept_alert().await.unwrap();
    until("a second tab", || async { owner.driver.windows().await.map(|w| w.len() == 2).unwrap_or(false) }).await;
    let tabs = owner.driver.windows().await.unwrap();
    owner.driver.switch_to_window(tabs[1].clone()).await.unwrap();
    until("the new tab to load", || async { owner.js("return !!window.__waypoint").await == json!(true) }).await;
    assert!(owner.role_label().await.starts_with("owner"));
    let stored = owner.js("return localStorage.getItem('waypoint.key')").await;
    let stored = stored.as_str().unwrap();
    assert!(stored.starts_with("wp_owner_") && stored != keys.owner, "a different owner key: {stored}");
    assert!(text.contains(stored), "the key shown is the key in use");
    assert!(owner.node_ids().await.is_empty(), "the new graph is empty");
    until("the title", || async { owner.js("return document.getElementById('doc-title').textContent").await == json!("Plan B") }).await;
}
