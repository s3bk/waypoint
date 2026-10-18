mod common;

use common::{Stack, until};
use serde_json::json;
use thirtyfour::By;

async fn editor_text(page: &common::Page) -> String {
    page.js("const e = document.querySelector('#margin .note-editor'); return e ? e.textContent : ''").await.as_str().unwrap_or("").to_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn each_node_has_one_shared_note_edited_in_the_margin() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let page = stack.page(&keys.editor).await;
    let note = || stack.call(&keys, &keys.viewer, "GET", "/nodes/%231", None).1["note"].clone();

    page.dblclick(300, 300).await;
    page.type_title("alpha").await;
    until("alpha on the server", || async { stack.node_count(&keys) == 1 }).await;
    until("alpha in the client", || async { page.id_titled("alpha").await.is_some() }).await;
    let alpha = page.id_titled("alpha").await.expect("alpha");
    page.press(thirtyfour::Key::Escape).await; // typing the title left it selected

    // Nothing selected: no margin. Select the node: its editor opens.
    let displayed = || async { page.js("return getComputedStyle(document.getElementById('margin')).display").await };
    assert_eq!(displayed().await, "none");
    let (cx, cy) = page.rect(&alpha).await.center();
    page.click(cx, cy).await;
    until("margin open", || async { displayed().await == "flex" }).await;

    // Typing goes straight to the server, as one text shared with everyone.
    let editor = page.driver.find(By::Css("#margin .note-editor")).await.unwrap();
    editor.click().await.unwrap();
    editor.send_keys("remember the milk").await.unwrap();
    until("the note on the server", || async { note() == json!("remember the milk") }).await;

    // An agent appends; it shows up in the open editor, and our caret stays where it was.
    let add = json!({ "ops": [{ "op": "append_note", "node": "#1", "text": "from an agent" }] });
    let sent = stack.call(&keys, &keys.editor, "POST", "/apply", Some(add));
    assert_eq!(sent.0, 200, "{}", sent.1);
    until("the agent's text in the editor", || async { editor_text(&page).await.ends_with("from an agent") }).await;
    editor.send_keys("!").await.unwrap(); // typed at the caret, which stayed before the agent's text
    until("both edits on the server", || async { note() == json!("remember the milk!\n\nfrom an agent") }).await;

    // A viewer sees the same text and cannot change it.
    let viewer = stack.page(&keys.viewer).await;
    until("the viewer's node", || async { viewer.id_titled("alpha").await.is_some() }).await;
    let id = viewer.id_titled("alpha").await.unwrap();
    let (vx, vy) = viewer.rect(&id).await.center();
    viewer.click(vx, vy).await;
    until("the viewer's margin", || async { editor_text(&viewer).await.contains("from an agent") }).await;
    let readonly = viewer.js("return document.querySelector('#margin .note-editor').isContentEditable").await;
    assert_eq!(readonly, json!(false));
}
