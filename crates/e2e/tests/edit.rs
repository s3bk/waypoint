mod common;

use common::{Stack, until};
use thirtyfour::Key;

#[tokio::test(flavor = "multi_thread")]
async fn editing_in_the_browser_reaches_the_server() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let page = stack.page(&keys.editor).await;
    let titles = || stack.call(&keys, &keys.viewer, "GET", "/nodes", None).1["nodes"].clone();
    let has_title = |t: &str| titles().as_array().is_some_and(|n| n.iter().any(|n| n["title"] == t));

    // Add a node by double-clicking empty canvas.
    page.dblclick(400, 300).await;
    page.type_title("alpha").await;
    until("alpha on the server", || async { has_title("alpha") }).await;
    assert_eq!(titles()[0]["key"], 1, "the server assigns the short key");
    until("alpha in the client", || async { page.id_titled("alpha").await.is_some() }).await;
    let alpha = page.id_titled("alpha").await.expect("alpha in the client");

    // Ctrl+Enter adds a dependant that depends on the selected node.
    page.press_ctrl(Key::Enter).await;
    page.type_title("beta").await;
    until("beta linked to alpha", || async {
        stack.call(&keys, &keys.viewer, "GET", "/nodes/%232", None).1["depends_on"].as_array().is_some_and(|d| d.len() == 1)
    })
    .await;
    let beta = stack.call(&keys, &keys.viewer, "GET", "/nodes/%232", None).1;
    assert_eq!(beta["depends_on"][0]["title"], "alpha");

    // Rename by double-clicking; the box opens with the title selected.
    let (cx, cy) = page.rect(&alpha).await.center();
    page.dblclick(cx, cy).await;
    until("editing alpha", || async { page.editing().await.as_deref() == Some(alpha.as_str()) }).await;
    page.type_title("gamma").await;
    until("renamed", || async { has_title("gamma") }).await;
    until("renamed in the client", || async { page.id_titled("gamma").await.is_some() }).await;
    assert_eq!(page.id_titled("gamma").await.as_deref(), Some(alpha.as_str()));
    let gamma = titles().as_array().unwrap().iter().find(|n| n["title"] == "gamma").unwrap().clone();
    assert_eq!(gamma["key"], 1, "renaming keeps the key");

    // Escape cancels an edit.
    page.dblclick(cx, cy).await;
    until("editing again", || async { page.editing().await.is_some() }).await;
    let editor = page.driver.find(thirtyfour::By::Id("editor")).await.unwrap();
    editor.send_keys(format!("zzz{}", char::from(Key::Escape))).await.unwrap();
    until("the box to close", || async { page.editing().await.is_none() }).await;
    assert_eq!(page.title(&alpha).await, "gamma");

    // Dragging moves the node.
    let before = page.rect(&alpha).await;
    let (cx, cy) = before.center();
    page.drag((cx, cy), (cx + 120, cy + 40)).await;
    until("the node to move", || async {
        let r = page.rect(&alpha).await;
        (r.x - before.x - 120).abs() <= 2 && (r.y - before.y - 40).abs() <= 2
    })
    .await;

    // Link two nodes by dragging from a side dot: delta depends on gamma.
    page.dblclick(640, 640).await;
    page.type_title("delta").await;
    until("delta in the client", || async { page.id_titled("delta").await.is_some() }).await;
    let delta = page.id_titled("delta").await.expect("delta");
    let d = page.rect(&delta).await;
    let target = page.rect(&alpha).await.center();
    page.drag((d.x, d.y + d.h / 2), target).await;
    until("delta linked", || async {
        stack.call(&keys, &keys.viewer, "GET", "/nodes/%233", None).1["depends_on"].as_array().is_some_and(|d| d.len() == 1)
    })
    .await;
    assert_eq!(stack.call(&keys, &keys.viewer, "GET", "/nodes/%233", None).1["depends_on"][0]["title"], "gamma");

    // Select a node and delete it: the others stay.
    let (cx, cy) = page.rect(&alpha).await.center();
    page.click(cx, cy).await;
    assert_eq!(page.selected_node().await.as_deref(), Some(alpha.as_str()));
    page.press(Key::Delete).await;
    until("gamma deleted", || async { !has_title("gamma") }).await;
    assert_eq!(titles().as_array().unwrap().len(), 2);

    // Ctrl+Z undoes the delete; Ctrl+Shift+Z is not needed to see it return.
    page.press_ctrl_char('z').await;
    until("gamma restored by undo", || async { has_title("gamma") }).await;
    assert_eq!(titles().as_array().unwrap().len(), 3);
}
