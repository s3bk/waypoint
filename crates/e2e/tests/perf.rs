mod common;

use common::Stack;
use serde_json::{Value, json};

/// Manual benchmark: `WAYPOINT_PERF_GRAPH=full.json cargo test -p waypoint-e2e --test perf -- --ignored --nocapture`,
/// with a `GET /full` dump of a real graph. Replays its nodes and links, then times zooming and hovering.
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn zooming_and_hovering_a_real_graph() {
    let Ok(path) = std::env::var("WAYPOINT_PERF_GRAPH") else { return };
    let dump: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut nodes: Vec<&Value> = dump["nodes"].as_array().unwrap().iter().collect();
    nodes.sort_by_key(|n| n["key"].as_u64());
    let mut ops: Vec<Value> = nodes.iter().map(|n| json!({ "op": "create_node", "title": n["title"] })).collect();
    let index = |r: &Value| nodes.iter().position(|n| format!("#{}", n["key"]) == r.as_str().unwrap()).unwrap();
    for l in dump["links"].as_array().unwrap() {
        ops.push(json!({ "op": "link", "from": format!("${}", index(&l["from"])), "to": format!("${}", index(&l["to"])) }));
    }
    let stack = Stack::start();
    let keys = stack.new_graph();
    for chunk in ops.chunks(100) {
        let (status, body) = stack.call(&keys, &keys.editor, "POST", "/apply", Some(json!({ "ops": chunk })));
        assert_eq!(status, 200, "{body}");
    }
    let page = stack.page(&keys.viewer).await;
    let r = page
        .js(
            r#"const c = document.querySelector('#canvas canvas');
               const t = (n, f) => { const s = performance.now(); for (let i = 0; i < n; i++) f(i); return (performance.now() - s) / n; };
               const wheel = t(30, (i) => c.dispatchEvent(new WheelEvent('wheel', { deltaY: i % 2 ? 100 : -100, clientX: 400, clientY: 300, offsetX: 400, offsetY: 300, bubbles: true, cancelable: true })));
               const move = t(30, (i) => window.dispatchEvent(new PointerEvent('pointermove', { clientX: 200 + i * 7, clientY: 300, bubbles: true })));
               return { wheel_ms: wheel, pointermove_ms: move };"#,
        )
        .await;
    println!("{r}");
    if let Ok(dir) = std::env::var("WAYPOINT_PERF_SHOT") {
        page.driver.screenshot(std::path::Path::new(&dir)).await.unwrap();
    }
}
