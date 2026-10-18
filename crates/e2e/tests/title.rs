mod common;

use common::{Stack, until};
use serde_json::json;
use thirtyfour::By;

async fn shown_title(page: &common::Page) -> String {
    page.js("return document.getElementById('doc-title').textContent").await.as_str().unwrap_or("").to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_names_the_graph_and_the_browser_remembers_it() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let owner = stack.page(&keys.owner).await;
    let editor = stack.page(&keys.editor).await;
    assert!(shown_title(&owner).await.starts_with("Unnamed"));

    // The owner renames by clicking the title; everyone sees it live.
    owner.driver.find(By::Id("doc-title")).await.unwrap().click().await.unwrap();
    until("the title input", || async { owner.driver.find(By::Css("#doc-title input")).await.is_ok() }).await;
    let input = owner.driver.find(By::Css("#doc-title input")).await.unwrap();
    input.send_keys("Launch plan\n").await.unwrap();
    until("the owner's title", || async { shown_title(&owner).await == "Launch plan" }).await;
    until("the editor's title", || async { shown_title(&editor).await == "Launch plan" }).await;
    assert_eq!(stack.call(&keys, &keys.viewer, "GET", "", None).1["title"], "Launch plan");

    // An editor key may not change it, and the editor's page offers no way to.
    let body = json!({ "title": "mine" });
    assert_eq!(stack.call(&keys, &keys.editor, "PUT", "/title", Some(body.clone())).0, 403);
    assert_eq!(stack.call(&keys, &keys.owner, "PUT", "/title", Some(json!({ "title": "  " }))).0, 400);

    // The (title, key) pair is in localStorage; with no current key, the start dialog offers it.
    let saved = owner.js("return JSON.parse(localStorage.getItem('waypoint.graphs'))").await;
    assert_eq!((saved[0]["title"].clone(), saved[0]["key"].clone()), (json!("Launch plan"), json!(keys.owner)));
    owner.js("localStorage.removeItem('waypoint.key'); location.href = location.origin + '/'").await;
    let saved_button = || owner.driver.find(By::Css("#start .saved-graph"));
    until("the start dialog", || async { saved_button().await.is_ok() }).await;
    let open = saved_button().await.unwrap();
    assert_eq!(open.text().await.unwrap(), "Launch plan · owner");
    open.click().await.unwrap();
    until("the graph to open again", || async { owner.js("return !!window.__waypoint").await == json!(true) }).await;
    until("its title", || async { shown_title(&owner).await == "Launch plan" }).await;

    // The editor's key is kept too, but it never replaces the owner's.
    let kept = editor.js("return JSON.parse(localStorage.getItem('waypoint.graphs'))[0]").await;
    assert_eq!((kept["role"].clone(), kept["title"].clone()), (json!("editor"), json!("Launch plan")));
}
