//! Read side of the agent API: JSON views of a graph snapshot. Nodes are
//! always shown with their key, so an agent can name them in later calls.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::{Value, json};
use waypoint_layout::derive_positions;
use waypoint_model::NodeId;

use crate::graph::{NodeRec, Snapshot};
use crate::ops::resolve_ref;

const MAX_NEIGHBOURS: usize = 200;
const MAX_DEPTH: usize = 5;

fn brief(n: &NodeRec) -> Value {
    json!({ "id": n.id.0, "key": n.key, "ref": n.key.map(|k| format!("#{k}")), "title": n.title })
}

/// The type registries and the canvas colour: what an agent needs to style things.
pub fn types(snap: &Snapshot) -> Value {
    json!({ "background": snap.background, "node_types": snap.node_types, "edge_types": snap.edge_types })
}

pub fn summary(snap: &Snapshot) -> Value {
    json!({ "title": snap.title, "nodes": snap.nodes.len(), "links": snap.edges.len(), "conflicts": snap.conflicted.len() })
}

/// The whole graph in one response: every node (key, title, note, position),
/// every link as `from`/`to` refs ("from is the source, to the destination"), and set-aside links.
pub fn full(snap: &Snapshot) -> Value {
    let pos = derive_positions(&snap.places());
    let label = |id: NodeId| snap.nodes[&id].key.map(|k| format!("#{k}")).unwrap_or_else(|| id.0.to_string());
    let mut nodes: Vec<&NodeRec> = snap.nodes.values().collect();
    nodes.sort_by_key(|n| (n.key.is_none(), n.key, n.id));
    let nodes: Vec<Value> = nodes
        .into_iter()
        .map(|n| {
            let mut v = brief(n);
            v["note"] = json!(n.note);
            v["node_type"] = json!(snap.node_type(n.type_id).name);
            v["position"] = json!({ "x": pos[&n.id].x, "y": pos[&n.id].y });
            v
        })
        .collect();
    let links = |edges: &[crate::graph::EdgeRec]| -> Vec<Value> {
        edges.iter().map(|e| json!({ "from": label(e.from), "to": label(e.to), "edge_type": snap.edge_type(e.type_id).name })).collect()
    };
    json!({
        "title": snap.title,
        "nodes": nodes,
        "links": links(&snap.edges),
        "conflicts": links(&snap.conflicted),
        "types": types(snap),
        "meaning": "a link from source to destination",
    })
}

/// Links the server set aside because two people's changes together closed a
/// cycle. They are not in any other result; `unlink_nodes` removes one.
pub fn conflicts(snap: &Snapshot) -> Value {
    let label = |id: NodeId| snap.nodes[&id].key.map(|k| format!("#{k}")).unwrap_or_else(|| id.0.to_string());
    let title = |id: NodeId| snap.nodes[&id].title.clone();
    json!({
        "conflicts": snap.conflicted.iter().map(|e| json!({
            "from": label(e.from), "from_title": title(e.from),
            "to": label(e.to), "to_title": title(e.to),
            "meaning": "from is the source and to the destination; this link would close a cycle",
        })).collect::<Vec<_>>(),
    })
}

/// Nodes whose title contains `query` (case-insensitive), or whose key is `query`
/// (`"#12"`/`"12"`), ordered by key.
pub fn search(snap: &Snapshot, query: Option<&str>, limit: usize) -> Value {
    let q = query.map(|q| q.trim().to_lowercase()).filter(|q| !q.is_empty());
    let key = q.as_deref().and_then(|q| q.strip_prefix('#').unwrap_or(q).parse::<u64>().ok());
    let mut hits: Vec<&NodeRec> = snap
        .nodes
        .values()
        .filter(|n| match &q {
            None => true,
            Some(q) => n.title.to_lowercase().contains(q.as_str()) || (key.is_some() && n.key == key),
        })
        .collect();
    hits.sort_by_key(|n| (n.key.is_none(), n.key, n.id));
    let total = hits.len();
    json!({ "total": total, "nodes": hits.into_iter().take(limit).map(brief).collect::<Vec<_>>() })
}

pub fn node(snap: &Snapshot, reference: &str) -> Result<Value, String> {
    let id = resolve_ref(&snap.nodes, &[], reference)?;
    let n = &snap.nodes[&id];
    let pos = derive_positions(&snap.places());
    let list = |pick: &dyn Fn(&crate::graph::EdgeRec) -> Option<NodeId>| -> Vec<Value> {
        let mut out: Vec<&NodeRec> = snap.edges.iter().filter_map(pick).filter_map(|o| snap.nodes.get(&o)).collect();
        out.sort_by_key(|n| (n.key.is_none(), n.key, n.id));
        out.into_iter().map(brief).collect()
    };
    let mut v = brief(n);
    v["position"] = json!({ "x": pos[&id].x, "y": pos[&id].y });
    v["destinations"] = Value::Array(list(&|e| (e.from == id).then_some(e.to)));
    v["sources"] = Value::Array(list(&|e| (e.to == id).then_some(e.from)));
    v["note"] = json!(n.note);
    v["node_type"] = json!(snap.node_type(n.type_id).name);
    Ok(v)
}

/// The graph in Graphviz DOT, for `dot -Tsvg` or for reading. An arrow
/// `A -> B` means "A links to B". Set-aside (conflicted) links are dashed red.
pub fn dot(snap: &Snapshot) -> String {
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " "));
    let name = |id: NodeId| q(&id.0.to_string());
    let mut out = String::from("digraph waypoint {\n  rankdir=RL; // an arrow A -> B means \"A links to B\"\n  node [shape=box, style=rounded];\n");
    let mut nodes: Vec<&NodeRec> = snap.nodes.values().collect();
    nodes.sort_by_key(|n| (n.key.is_none(), n.key, n.id));
    for n in nodes {
        let label = match n.key {
            Some(k) => format!("#{k} {}", n.title),
            None => n.title.clone(),
        };
        let label = if n.note.trim().is_empty() { label } else { format!("{label} (has a note)") };
        out.push_str(&format!("  {} [label={}];\n", name(n.id), q(&label)));
    }
    for e in &snap.edges {
        out.push_str(&format!("  {} -> {};\n", name(e.from), name(e.to)));
    }
    for e in &snap.conflicted {
        out.push_str(&format!("  {} -> {} [style=dashed, color=red];\n", name(e.from), name(e.to)));
    }
    out.push_str("}\n");
    out
}

/// The subgraph within `depth` links of a node. `direction` is `"destinations"`
/// (what it links to, transitively), `"sources"` (what links to it) or `"both"`.
/// The old `"dependencies"` and `"dependants"` mean the same.
pub fn neighbours(snap: &Snapshot, reference: &str, direction: &str, depth: usize) -> Result<Value, String> {
    let root = resolve_ref(&snap.nodes, &[], reference)?;
    let (down, up) = match direction {
        "destinations" | "dependencies" => (true, false),
        "sources" | "dependants" => (false, true),
        "both" => (true, true),
        other => return Err(format!("direction must be \"destinations\", \"sources\" or \"both\", not {other:?}")),
    };
    let depth = depth.clamp(1, MAX_DEPTH);
    let mut next: BTreeMap<NodeId, Vec<NodeId>> = BTreeMap::new();
    for e in &snap.edges {
        if down {
            next.entry(e.from).or_default().push(e.to);
        }
        if up {
            next.entry(e.to).or_default().push(e.from);
        }
    }
    let mut dist: BTreeMap<NodeId, usize> = BTreeMap::from([(root, 0)]);
    let mut queue = VecDeque::from([root]);
    let mut truncated = false;
    while let Some(n) = queue.pop_front() {
        let d = dist[&n];
        if d == depth {
            continue;
        }
        for &m in next.get(&n).into_iter().flatten() {
            if dist.contains_key(&m) {
                continue;
            }
            if dist.len() >= MAX_NEIGHBOURS {
                truncated = true;
                continue;
            }
            dist.insert(m, d + 1);
            queue.push_back(m);
        }
    }
    let included: BTreeSet<NodeId> = dist.keys().copied().collect();
    let mut nodes: Vec<(usize, &NodeRec)> = dist.iter().map(|(id, d)| (*d, &snap.nodes[id])).collect();
    nodes.sort_by_key(|(d, n)| (*d, n.key.is_none(), n.key, n.id));
    let label = |id: NodeId| snap.nodes[&id].key.map(|k| format!("#{k}")).unwrap_or_else(|| id.0.to_string());
    Ok(json!({
        "root": brief(&snap.nodes[&root]),
        "nodes": nodes.into_iter().map(|(d, n)| { let mut v = brief(n); v["distance"] = json!(d); v }).collect::<Vec<_>>(),
        "links": snap.edges.iter().filter(|e| included.contains(&e.from) && included.contains(&e.to))
            .map(|e| json!({ "from": label(e.from), "to": label(e.to), "meaning": "from is the source, to the destination" })).collect::<Vec<_>>(),
        "truncated": truncated,
    }))
}
