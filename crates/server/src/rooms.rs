//! One `Room` per graph: the in-memory document, the relay that fans client
//! updates out to the other connections, and the link to storage. Rooms are
//! loaded from SQLite on first use and kept in memory.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
};

use tokio::sync::broadcast;
use waypoint_model::Batch;
use yrs::{
    sync::{Message, protocol::SyncMessage},
    updates::encoder::Encode,
};

use crate::{graph::{Graph, Write}, ops, ops::OpError, repair, store::Store, sync::Relay};

/// Fold the update log into a snapshot after this many updates.
const COMPACT_AFTER: u32 = 200;

pub struct Room {
    pub id: String,
    pub graph: Graph,
    pub relay: broadcast::Sender<Relay>,
    store: Arc<Store>,
    /// Serialises server-made edits, so a batch plans against a state no other batch is changing.
    edit_lock: Mutex<()>,
    /// Updates written since the last snapshot.
    unsaved: Arc<AtomicU32>,
    /// The graph's title as the owner set it. Clients with an editor key can write
    /// anywhere in the document, so after each client update this is written back
    /// if the document disagrees.
    title: Mutex<String>,
}

const MAX_GRAPH_TITLE: usize = 80;

/// Whitespace collapsed, at most 80 characters, not empty.
pub fn normalize_graph_title(title: &str) -> Result<String, String> {
    let t: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = t.chars().take(MAX_GRAPH_TITLE).collect();
    if t.is_empty() { Err("the title must not be empty".into()) } else { Ok(t) }
}

impl Room {
    fn open(id: &str, store: Arc<Store>) -> Result<Arc<Self>, String> {
        let stored = store.load(id).map_err(|e| e.to_string())?;
        let graph = Graph::new();
        if let Some(snapshot) = &stored.snapshot {
            graph.apply_update(snapshot)?;
        }
        for update in &stored.updates {
            graph.apply_update(update)?;
        }

        // Subscribe only now, so loading does not write itself back.
        let unsaved = Arc::new(AtomicU32::new(stored.updates.len() as u32));
        {
            let (store, id, unsaved) = (store.clone(), id.to_owned(), unsaved.clone());
            graph
                .doc()
                .observe_update_v1_with("persist", move |_, e| {
                    store.append(&id, e.update.clone());
                    unsaved.fetch_add(1, Ordering::Relaxed);
                })
                .map_err(|e| e.to_string())?;
        }

        if id == "demo" && graph.is_empty() {
            graph.seed_demo();
        }

        let (relay, _) = broadcast::channel(1024);
        let title = Mutex::new(graph.snapshot().title);
        let room = Arc::new(Self { id: id.to_owned(), graph, relay, store, edit_lock: Mutex::new(()), unsaved, title });
        room.assign_keys();
        room.repair(); // a stale client may have re-merged something invalid before the last shutdown
        room.maybe_compact();
        Ok(room)
    }

    /// Send a document update to every connection, including the one that
    /// caused it (relay id 0 is the server itself).
    fn broadcast(&self, update: Vec<u8>) {
        let frame = Message::Sync(SyncMessage::Update(update)).encode_v1();
        let _ = self.relay.send(Relay { from: 0, frame });
    }

    /// Number any nodes that lack a key and tell everyone. Call after changes
    /// that may have added nodes.
    pub fn assign_keys(&self) {
        if let Some(update) = self.graph.assign_keys() {
            self.broadcast(update);
        }
    }

    /// Fix what a merge can leave invalid (cycles, anchors) and tell everyone.
    /// Call after applying a client's update.
    pub fn repair(&self) {
        let _editing = self.edit_lock.lock().expect("edit lock");
        self.repair_locked();
    }

    fn repair_locked(&self) {
        let snapshot = self.graph.snapshot();
        let mut writes = repair::plan(&snapshot);
        let title = self.title.lock().expect("title lock").clone();
        if snapshot.title != title {
            writes.push(Write::SetGraphTitle { title }); // someone without the owner key changed it
        }
        if !writes.is_empty() {
            self.broadcast(self.graph.apply_writes(&writes));
        }
    }

    /// Apply a batch of agent operations atomically: all of it, or none. The
    /// changes reach connected clients like anyone else's edits.
    ///
    /// `actor` names who is acting: the batch goes into that actor's undo
    /// history and the change log.
    pub fn apply(&self, batch: &Batch, actor: &str) -> Result<Vec<serde_json::Value>, OpError> {
        let _editing = self.edit_lock.lock().expect("edit lock");
        self.assign_keys(); // so nodes added by clients can be named by key
        let plan = ops::plan(&self.graph.snapshot(), &batch.ops)?;
        if !plan.writes.is_empty() {
            let ops: Vec<_> = plan.results.iter().map(|r| r["op"].clone()).collect();
            let entry = log_entry(actor, "apply", batch.label.as_deref(), serde_json::json!(ops));
            self.broadcast(self.graph.apply_writes_as(actor, entry, &plan.writes));
        }
        self.assign_keys();
        let after = self.graph.snapshot();
        let mut results = plan.results;
        for r in &mut results {
            if r["op"] == "create_node" {
                let key = r["id"].as_str().and_then(|id| id.parse().ok()).and_then(|u| after.nodes.get(&waypoint_model::NodeId(u))).and_then(|n| n.key);
                r["key"] = serde_json::json!(key);
                r["ref"] = serde_json::json!(key.map(|k| format!("#{k}")));
            }
        }
        self.maybe_compact();
        Ok(results)
    }

    /// Set the graph's title (the caller has checked that they hold the owner key).
    pub fn set_title(&self, actor: &str, title: &str) -> Result<String, String> {
        let title = normalize_graph_title(title)?;
        let _editing = self.edit_lock.lock().expect("edit lock");
        *self.title.lock().expect("title lock") = title.clone();
        let entry = log_entry(actor, "title", Some(&title), serde_json::json!([]));
        self.broadcast(self.graph.apply_writes_as(actor, entry, &[Write::SetGraphTitle { title: title.clone() }]));
        Ok(title)
    }

    /// Undo or redo the actor's latest batch. `false` if there was nothing to do.
    pub fn undo(&self, actor: &str, redo: bool) -> bool {
        let _editing = self.edit_lock.lock().expect("edit lock");
        let entry = log_entry(actor, if redo { "redo" } else { "undo" }, None, serde_json::json!([]));
        match self.graph.undo_as(actor, redo, entry) {
            Some(update) => {
                self.broadcast(update);
                self.repair_locked();
                true
            }
            None => false,
        }
    }

    /// Replace the update log with a snapshot once it has grown long enough.
    /// Call outside any document transaction.
    pub fn maybe_compact(&self) {
        if self.unsaved.load(Ordering::Relaxed) < COMPACT_AFTER {
            return;
        }
        self.unsaved.store(0, Ordering::Relaxed);
        // Order matters: read the sequence number first (see Store::last_seq).
        match self.store.last_seq(&self.id) {
            Ok(seq) => self.store.snapshot(&self.id, seq, self.graph.encode_state()),
            Err(e) => eprintln!("room {}: compaction skipped: {e}", self.id),
        }
    }
}

/// One change-log entry, as the JSON string stored in the document.
fn log_entry(actor: &str, kind: &str, label: Option<&str>, ops: serde_json::Value) -> String {
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    serde_json::json!({ "actor": actor, "kind": kind, "label": label, "ops": ops, "at": at }).to_string()
}

pub struct Rooms {
    pub store: Arc<Store>,
    rooms: Mutex<HashMap<String, Arc<Room>>>,
}

impl Rooms {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store, rooms: Mutex::new(HashMap::new()) }
    }

    /// Whether the graph has ever been written to (or is open right now).
    pub fn exists(&self, id: &str) -> bool {
        self.rooms.lock().expect("rooms lock").contains_key(id) || self.store.has_graph(id)
    }

    /// The room for a graph, loading it from storage if this is its first use.
    pub fn get(&self, id: &str) -> Result<Arc<Room>, String> {
        let mut rooms = self.rooms.lock().expect("rooms lock");
        if let Some(room) = rooms.get(id) {
            return Ok(room.clone());
        }
        let room = Room::open(id, self.store.clone())?;
        rooms.insert(id.to_owned(), room.clone());
        Ok(room)
    }
}

/// Stored graph ids are hex digests; this is the shape check the tests use.
#[cfg(test)]
pub fn valid_graph_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use waypoint_model::{NodeId, Place, Vec2};

    fn rooms_at(path: &std::path::Path) -> Rooms {
        Rooms::new(Arc::new(Store::open(path).unwrap()))
    }

    fn add_nodes(room: &Room, n: usize) {
        let row = room.graph.places().len() as i32 * 1000; // clear of earlier calls
        for i in 0..n {
            room.graph.create_node(NodeId::new(), &format!("n{i}"), Place::at_origin(Vec2::new(i as i32 * 1000, row)));
        }
    }

    #[test]
    fn only_the_owner_route_sets_the_title_and_it_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");
        let rooms = rooms_at(&db);
        let room = rooms.get("g1").unwrap();
        assert!(room.set_title("owner", "   ").is_err());
        assert_eq!(room.set_title("owner", "  Launch   plan ").unwrap(), "Launch plan");

        // An editor's client writes the title straight into the document: it is put back.
        let rogue = Graph::new();
        rogue.apply_writes(&[Write::SetGraphTitle { title: "hijacked".into() }]);
        room.graph.apply_update(&rogue.encode_state()).unwrap();
        room.repair();
        assert_eq!(room.graph.snapshot().title, "Launch plan");

        rooms.store.flush();
        drop((room, rooms));
        assert_eq!(rooms_at(&db).get("g1").unwrap().graph.snapshot().title, "Launch plan");
    }

    #[test]
    fn graph_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");

        let rooms = rooms_at(&db);
        let room = rooms.get("g1").unwrap();
        add_nodes(&room, 3);
        let before = room.graph.places();
        rooms.store.flush();
        drop((room, rooms));

        let reopened = rooms_at(&db).get("g1").unwrap();
        assert_eq!(reopened.graph.places(), before);
        assert_eq!(before.len(), 3);
    }

    #[test]
    fn graphs_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let rooms = rooms_at(&dir.path().join("w.db"));
        add_nodes(&rooms.get("a").unwrap(), 2);
        add_nodes(&rooms.get("b").unwrap(), 5);
        assert_eq!(rooms.get("a").unwrap().graph.places().len(), 2);
        assert_eq!(rooms.get("b").unwrap().graph.places().len(), 5);
        rooms.store.flush();
        assert_eq!(rooms.store.graph_ids().unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn demo_is_seeded_once_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");
        for _ in 0..3 {
            let rooms = rooms_at(&db);
            assert_eq!(rooms.get("demo").unwrap().graph.places().len(), 3);
            rooms.store.flush();
        }
    }

    #[test]
    fn compaction_shrinks_the_log_and_loses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");

        let rooms = rooms_at(&db);
        let room = rooms.get("big").unwrap();
        add_nodes(&room, (COMPACT_AFTER as usize) + 50);
        rooms.store.flush(); // the log must be on disk before it can be compacted
        room.maybe_compact();
        add_nodes(&room, 10); // edits after the snapshot must survive too
        rooms.store.flush();
        let expected = room.graph.places();

        let stored = rooms.store.load("big").unwrap();
        assert!(stored.snapshot.is_some());
        assert!(stored.updates.len() < 50, "log not compacted: {} updates", stored.updates.len());
        drop((room, rooms));

        let reopened = rooms_at(&db).get("big").unwrap();
        assert_eq!(reopened.graph.places(), expected);
        assert_eq!(expected.len(), COMPACT_AFTER as usize + 60);
    }

    fn batch(ops: serde_json::Value) -> Batch {
        serde_json::from_value(serde_json::json!({ "ops": ops })).unwrap()
    }

    #[test]
    fn an_applied_batch_reaches_clients_gets_keys_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");
        let rooms = rooms_at(&db);
        let room = rooms.get("agent").unwrap();
        let mut client = room.relay.subscribe(); // stands in for a connected browser

        let results = room
            .apply(&batch(serde_json::json!([
                { "op": "create_node", "title": "design" },
                { "op": "create_node", "title": "build", "depends_on": ["$0"] },
            ])), "tester")
            .unwrap();
        assert_eq!((results[0]["key"].as_u64(), results[1]["key"].as_u64()), (Some(1), Some(2)));
        assert_eq!(results[1]["ref"], "#2");

        let first = client.try_recv().expect("clients are told about the change");
        assert_eq!(first.from, 0, "it came from the server");
        assert!(client.try_recv().is_ok(), "and then about the keys");

        // A failed batch changes nothing and tells nobody.
        let err = room.apply(&batch(serde_json::json!([{ "op": "link", "from": "#1", "to": "#2" }])), "tester").unwrap_err();
        assert!(err.message.contains("cycle"));
        assert!(client.try_recv().is_err());

        let expected = room.graph.snapshot();
        rooms.store.flush();
        drop((client, room, rooms));
        let reopened = rooms_at(&db).get("agent").unwrap().graph.snapshot();
        assert_eq!(reopened.nodes, expected.nodes);
        assert_eq!(reopened.edges, expected.edges);
        assert_eq!(reopened.nodes.values().filter_map(|n| n.key).max(), Some(2));
    }

    #[test]
    fn each_actor_undoes_only_their_own_batches_and_the_log_says_who() {
        let dir = tempfile::tempdir().unwrap();
        let room = rooms_at(&dir.path().join("w.db")).get("undo").unwrap();
        let add = |title: &str, actor: &str| {
            room.apply(&batch(serde_json::json!([{ "op": "create_node", "title": title }])), actor).unwrap();
        };
        let titles = || {
            let mut t: Vec<_> = room.graph.snapshot().nodes.values().map(|n| n.title.clone()).collect();
            t.sort();
            t
        };
        add("a1", "ann");
        add("b1", "bob");
        add("a2", "ann");
        assert!(room.undo("ann", false));
        assert_eq!(titles(), ["a1", "b1"], "ann's latest batch is gone, bob's stays");
        assert!(room.undo("ann", false));
        assert!(!room.undo("ann", false), "nothing left for ann");
        assert_eq!(titles(), ["b1"]);
        assert!(room.undo("bob", false));
        assert!(room.undo("bob", true), "redo");
        assert_eq!(titles(), ["b1"]);
        assert!(!room.undo("nobody", false));

        let log = room.graph.log();
        let who: Vec<_> = log.iter().map(|e| format!("{}:{}", e["actor"].as_str().unwrap(), e["kind"].as_str().unwrap())).collect();
        assert_eq!(who, ["ann:apply", "bob:apply", "ann:apply", "ann:undo", "ann:undo", "bob:undo", "bob:redo"]);
    }

    #[test]
    fn graph_ids_are_validated() {
        assert!(valid_graph_id("demo"));
        assert!(valid_graph_id("my-graph_2"));
        assert!(!valid_graph_id(""));
        assert!(!valid_graph_id("a/b"));
        assert!(!valid_graph_id("../x"));
        assert!(!valid_graph_id(&"x".repeat(65)));
    }
}
