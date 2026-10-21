//! WASM bindings. JSON in and out keeps the boundary simple while the API is
//! small; revisit (e.g. typed arrays) if profiling says so.

use std::collections::BTreeMap;

use wasm_bindgen::prelude::*;
use waypoint_model::{NodeId, Place, Vec2};

/// `places`: JSON object `{ "<node id>": { "anchor": "<id>"|null, "off": {x,y} } }`.
/// Returns JSON `{ "<node id>": {x,y} }`.
#[wasm_bindgen]
pub fn derive_positions(places: &str) -> Result<String, JsError> {
    let places: BTreeMap<NodeId, Place> = serde_json::from_str(places)?;
    Ok(serde_json::to_string(&crate::derive_positions(&places))?)
}

/// The places that change when `id` is dragged to `(x, y)`. Returns JSON
/// `[["<node id>", {anchor, off}], ...]`.
#[wasm_bindgen]
pub fn move_node(places: &str, id: &str, x: i32, y: i32, with_subtree: bool) -> Result<String, JsError> {
    let places: BTreeMap<NodeId, Place> = serde_json::from_str(places)?;
    let changes = crate::move_node(&places, parse_id(id)?, Vec2::new(x, y), with_subtree);
    Ok(serde_json::to_string(&changes)?)
}

/// Like `move_node`, but the node moves together with everything it links to.
/// `edges` is JSON `[["<from>", "<to>"], ...]`. Returns JSON `[[id, place], ...]`.
#[wasm_bindgen]
pub fn move_group(places: &str, edges: &str, id: &str, x: i32, y: i32) -> Result<String, JsError> {
    let places: BTreeMap<NodeId, Place> = serde_json::from_str(places)?;
    let changes = crate::move_group(&places, &parse_edges(edges)?, parse_id(id)?, Vec2::new(x, y));
    Ok(serde_json::to_string(&changes)?)
}

/// Add node `id` and make room. `parent` is a node id or "" for none; `(x, y)` is
/// used when there is no parent. Returns JSON `[["<id>", place], ...]` with the
/// new node's place and every place that was pushed.
#[wasm_bindgen]
pub fn add_node(places: &str, edges: &str, id: &str, parent: &str, x: i32, y: i32) -> Result<String, JsError> {
    let places: BTreeMap<NodeId, Place> = serde_json::from_str(places)?;
    let edges = parse_edges(edges)?;
    let id = parse_id(id)?;
    let parent = if parent.is_empty() { None } else { Some(parse_id(parent)?) };
    Ok(serde_json::to_string(&crate::add_node(&places, &edges, id, parent, Vec2::new(x, y)))?)
}

/// Like `move_node`, then makes room for whatever the drop landed on.
#[wasm_bindgen]
pub fn drop_node(places: &str, edges: &str, id: &str, x: i32, y: i32, with_subtree: bool) -> Result<String, JsError> {
    let places: BTreeMap<NodeId, Place> = serde_json::from_str(places)?;
    let changes = crate::drop_node(&places, &parse_edges(edges)?, parse_id(id)?, Vec2::new(x, y), with_subtree);
    Ok(serde_json::to_string(&changes)?)
}

fn parse_id(id: &str) -> Result<NodeId, JsError> {
    Ok(serde_json::from_value(serde_json::Value::String(id.to_owned()))?)
}

fn parse_edges(edges: &str) -> Result<Vec<(NodeId, NodeId)>, JsError> {
    Ok(serde_json::from_str(edges)?)
}

/// Re-anchor nodes whose anchor is no longer a neighbour. `edges` is JSON
/// `[["<from>", "<to>"], ...]` of the live edges. Returns JSON `[[id, place], ...]`.
#[wasm_bindgen]
pub fn repair_anchors(places: &str, edges: &str) -> Result<String, JsError> {
    let places: BTreeMap<NodeId, Place> = serde_json::from_str(places)?;
    Ok(serde_json::to_string(&crate::repair_anchors(&places, &parse_edges(edges)?))?)
}

/// Would a link from `from` to `to` close a cycle?
#[wasm_bindgen]
pub fn creates_cycle(edges: &str, from: &str, to: &str) -> Result<bool, JsError> {
    Ok(crate::creates_cycle(&parse_edges(edges)?, parse_id(from)?, parse_id(to)?))
}

/// Edges to add so chains through `node` survive its deletion. `edges` is JSON
/// `[["<from>", "<to>", "<type>"], ...]`; returns the same shape.
#[wasm_bindgen]
pub fn bridge_edges(edges: &str, node: &str) -> Result<String, JsError> {
    let edges: Vec<(NodeId, NodeId, waypoint_model::EdgeTypeId)> = serde_json::from_str(edges)?;
    Ok(serde_json::to_string(&crate::bridge_edges(&edges, parse_id(node)?))?)
}

/// Lay the whole graph out by `method` (`clustered`, `force`, `layered` or `polish`) without touching
/// anything: a preview. `nodes` is JSON `[["<id>", <type number>], ...]`, `edges` `[["<from>", "<to>", <label width px, 0 for none>], ...]`,
/// `current` `{ "<id>": {x, y} }` (top-left corners; used by `polish`). /// `bounds_w` x `bounds_h` is the area the force-directed methods must stay inside (0 for no limit).
/// Returns JSON `{ "positions": { "<id>": {x, y} }, "defects": <number> }`: top-left corners with the drawing's
/// top-left at the origin, and how many defects remain (0 means the drawing follows every rule).
/// Slow on big graphs: call it from a worker.
#[wasm_bindgen]
pub fn arrange(method: &str, nodes: &str, edges: &str, current: &str, aspect: f64, bounds_w: f64, bounds_h: f64) -> Result<String, JsError> {
    let method = crate::Method::parse(method).ok_or_else(|| JsError::new("unknown layout method"))?;
    let nodes: Vec<(NodeId, u32)> = serde_json::from_str(nodes)?;
    let current: BTreeMap<NodeId, Vec2> = serde_json::from_str(current)?;
    // `edges` here are `[from, to, label width in px]`; a width of 0 means the link has no label.
    let triples: Vec<(NodeId, NodeId, f64)> = serde_json::from_str(edges)?;
    let edges: Vec<(NodeId, NodeId)> = triples.iter().map(|&(f, t, _)| (f, t)).collect();
    let label_width: crate::Labels = triples.iter().filter(|e| e.2 > 0.0).map(|&(f, t, w)| ((f, t), w)).collect();
    let bounds = (bounds_w > 0.0 && bounds_h > 0.0).then_some((bounds_w, bounds_h));
    let positions = crate::arrange(method, &nodes, &edges, &current, aspect, &label_width, bounds);
    let defects = crate::defect_count(&nodes, &edges, &positions, &label_width);
    Ok(serde_json::to_string(&serde_json::json!({ "positions": positions, "defects": defects }))?)
}
