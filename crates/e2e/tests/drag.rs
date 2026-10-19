mod common;

use common::{Stack, until};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn drag_moves_one_node_and_shift_drag_takes_its_destinations() {
    let stack = Stack::start();
    let keys = stack.new_graph();
    let made = stack.call(
        &keys,
        &keys.editor,
        "POST",
        "/apply",
        Some(json!({ "ops": [
            { "op": "create_node", "title": "first" },
            { "op": "create_node", "title": "second", "destinations": ["$0"] },
        ] })),
    );
    assert_eq!(made.0, 200, "{}", made.1);
    let page = stack.page(&keys.editor).await;
    until("both nodes", || async { page.id_titled("second").await.is_some() && page.id_titled("first").await.is_some() }).await;
    let (first, second) = (page.id_titled("first").await.unwrap(), page.id_titled("second").await.unwrap());

    // "second" links to "first". Plain drag of "second": only it moves.
    let (a0, b0) = (page.rect(&first).await, page.rect(&second).await);
    let (cx, cy) = b0.center();
    page.drag((cx, cy), (cx, cy + 150)).await;
    until("second to move", || async { (page.rect(&second).await.y - b0.y - 150).abs() <= 2 }).await;
    let a1 = page.rect(&first).await;
    assert_eq!((a1.x, a1.y), (a0.x, a0.y), "first did not move");

    // Shift-drag of "second": it moves together with its destinations.
    let (a1, b1) = (page.rect(&first).await, page.rect(&second).await);
    let (cx, cy) = b1.center();
    page.drag_shift((cx, cy), (cx + 40, cy + 120)).await;
    until("second to move again", || async { (page.rect(&second).await.y - b1.y - 120).abs() <= 2 }).await;
    let a2 = page.rect(&first).await;
    assert_eq!((a2.x - a1.x, a2.y - a1.y), (40, 120), "first moved along");

    // Shift pressed in the middle of the drag counts too.
    let (a2, b2) = (page.rect(&first).await, page.rect(&second).await);
    let (cx, cy) = b2.center();
    page.drag_shift_midway((cx, cy), (cx - 30, cy + 90)).await;
    until("second to move a third time", || async { (page.rect(&second).await.y - b2.y - 90).abs() <= 2 }).await;
    let a3 = page.rect(&first).await;
    assert_eq!((a3.x - a2.x, a3.y - a2.y), (-30, 90), "first moved along");

    // Shift-dragging "first", which has no destinations, moves only it.
    let (a3, b3) = (page.rect(&first).await, page.rect(&second).await);
    let (cx, cy) = a3.center();
    page.drag_shift((cx, cy), (cx, cy - 60)).await;
    until("first to move", || async { (page.rect(&first).await.y - a3.y + 60).abs() <= 2 }).await;
    let b4 = page.rect(&second).await;
    assert_eq!((b4.x, b4.y), (b3.x, b3.y), "second stayed");
}
