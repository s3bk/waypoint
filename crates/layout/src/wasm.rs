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
