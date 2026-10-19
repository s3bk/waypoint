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

use crate::{
    crypt::{Cipher, Kind},
    graph::{Graph, Write},
    ops,
    ops::OpError,
    repair,
    store::{Blob, Store},
    sync::Relay,
};

/// Fold the update log into a snapshot after this many updates.
const COMPACT_AFTER: u32 = 200;

pub struct Room {
    pub id: String,
    pub graph: Graph,
    pub relay: broadcast::Sender<Relay>,
    store: Arc<Store>,
    /// Seals everything this room writes to `store`.
    cipher: Cipher,
    /// Serialises server-made edits, so a batch plans against a state no other batch is changing.
    edit_lock: Mutex<()>,
    /// Updates written since the last snapshot.
    unsaved: Arc<AtomicU32>,
    /// The graph's title as the owner set it. Clients with an editor key can write
    /// anywhere in the document, so after each client update this is written back
    /// if the document disagrees.
    title: Mutex<String>,
    /// The canvas colour, owner-only in the same way.
    background: Mutex<String>,
}

const MAX_GRAPH_TITLE: usize = 80;

/// Whitespace collapsed, at most 80 characters, not empty.
pub fn normalize_graph_title(title: &str) -> Result<String, String> {
    let t: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let t: String = t.chars().take(MAX_GRAPH_TITLE).collect();
    if t.is_empty() { Err("the title must not be empty".into()) } else { Ok(t) }
}

impl Room {
    fn open(id: &str, store: Arc<Store>, cipher: Cipher) -> Result<Arc<Self>, String> {
        let stored = store.load(id).map_err(|e| e.to_string())?;
        let graph = Graph::new();
        // Rows from before encryption are read as they are and sealed again below.
        let mut legacy = false;
        let mut read = |blob: &Blob, kind| -> Result<Vec<u8>, String> {
            legacy |= !blob.sealed;
            if blob.sealed { cipher.open(id, kind, &blob.data) } else { Ok(blob.data.clone()) }
        };
        if let Some(snapshot) = &stored.snapshot {
            graph.apply_update(&read(snapshot, Kind::Snapshot)?)?;
        }
        for update in &stored.updates {
            graph.apply_update(&read(update, Kind::Update)?)?;
        }

        // Subscribe only now, so loading does not write itself back.
        let unsaved = Arc::new(AtomicU32::new(stored.updates.len() as u32));
        {
            let (store, id, unsaved, cipher) = (store.clone(), id.to_owned(), unsaved.clone(), cipher.clone());
            graph
                .doc()
                .observe_update_v1_with("persist", move |_, e| {
                    store.append(&id, cipher.seal(&id, Kind::Update, &e.update));
                    unsaved.fetch_add(1, Ordering::Relaxed);
                })
                .map_err(|e| e.to_string())?;
        }

        if id == "demo" && graph.is_empty() {
            graph.seed_demo();
        }

        let (relay, _) = broadcast::channel(1024);
        let first = graph.snapshot();
        let (title, background) = (Mutex::new(first.title), Mutex::new(first.background));
        let room =
            Arc::new(Self { id: id.to_owned(), graph, relay, store, cipher, edit_lock: Mutex::new(()), unsaved, title, background });
        room.assign_keys();
        room.repair(); // a stale client may have re-merged something invalid before the last shutdown
        if legacy {
            room.seal_legacy(stored.last_seq);
        }
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
        let background = self.background.lock().expect("background lock").clone();
        if snapshot.background != background {
            writes.push(Write::SetBackground { color: background });
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
    pub fn apply(&self, batch: &Batch, actor: impl Into<Peer>) -> Result<Vec<serde_json::Value>, OpError> {
        let actor = &actor.into();
        let _editing = self.edit_lock.lock().expect("edit lock");
        self.assign_keys(); // so nodes added by clients can be named by key
        let plan = ops::plan(&self.graph.snapshot(), &batch.ops)?;
        if !plan.writes.is_empty() {
            let ops: Vec<_> = plan.results.iter().map(|r| r["op"].clone()).collect();
            let entry = log_entry(actor, "apply", batch.label.as_deref(), serde_json::json!(ops));
            self.broadcast(self.graph.apply_writes_as(&actor.id, entry, &plan.writes));
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
    pub fn set_title(&self, actor: impl Into<Peer>, title: &str) -> Result<String, String> {
        let actor = &actor.into();
        let title = normalize_graph_title(title)?;
        let _editing = self.edit_lock.lock().expect("edit lock");
        *self.title.lock().expect("title lock") = title.clone();
        let entry = log_entry(actor, "title", Some(&title), serde_json::json!([]));
        self.broadcast(self.graph.apply_writes_as(&actor.id, entry, &[Write::SetGraphTitle { title: title.clone() }]));
        Ok(title)
    }

    /// Set the canvas colour (the caller has checked the owner key). Edge colours and text
    /// must stay readable on it, so edge types that would not be are recoloured black or
    /// white in the same change. Returns the colour and the names of the types it changed.
    pub fn set_background(&self, actor: impl Into<Peer>, color: &str) -> Result<(String, Vec<String>), String> {
        let actor = &actor.into();
        use waypoint_model::style::{Kind, check_contrast, check_line_contrast, readable_on};
        let color = waypoint_model::style::normalize_color(color)?;
        let _editing = self.edit_lock.lock().expect("edit lock");
        let readable = readable_on(&color);
        let mut writes = vec![Write::SetBackground { color: color.clone() }];
        let mut changed = Vec::new();
        for t in &self.graph.snapshot().edge_types {
            let mut fields = Vec::new();
            if check_line_contrast("", &color, &t.color).is_err() {
                fields.push(("color".to_owned(), serde_json::json!(readable)));
            }
            if check_contrast("", &color, &t.text_color).is_err() {
                fields.push(("text_color".to_owned(), serde_json::json!(readable)));
            }
            if !fields.is_empty() {
                changed.push(t.name.clone());
                writes.push(Write::UpsertType { kind: Kind::Edge, id: t.id, fields });
            }
        }
        *self.background.lock().expect("background lock") = color.clone();
        let entry = log_entry(actor, "background", Some(&color), serde_json::json!([]));
        self.broadcast(self.graph.apply_writes_as(&actor.id, entry, &writes));
        Ok((color, changed))
    }

    /// Undo or redo the actor's latest batch. `false` if there was nothing to do.
    pub fn undo(&self, actor: impl Into<Peer>, redo: bool) -> bool {
        let actor = &actor.into();
        let _editing = self.edit_lock.lock().expect("edit lock");
        let entry = log_entry(actor, if redo { "redo" } else { "undo" }, None, serde_json::json!([]));
        match self.graph.undo_as(&actor.id, redo, entry) {
            Some(update) => {
                self.broadcast(update);
                self.repair_locked();
                true
            }
            None => false,
        }
    }

    /// Replace everything stored up to `last_seq` with one sealed snapshot, and purge the plaintext from the file.
    fn seal_legacy(&self, last_seq: i64) {
        self.unsaved.store(0, Ordering::Relaxed);
        self.store.snapshot(&self.id, last_seq, self.cipher.seal(&self.id, Kind::Snapshot, &self.graph.encode_state()));
        self.store.scrub();
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
            Ok(seq) => self.store.snapshot(&self.id, seq, self.cipher.seal(&self.id, Kind::Snapshot, &self.graph.encode_state())),
            Err(e) => eprintln!("room {}: compaction skipped: {e}", self.id),
        }
    }
}

/// Who is acting. The `id` is what identifies a peer (it keys the peer's undo history and tells
/// entries of the change log apart); the `name` is whatever the peer calls itself, for people to
/// read. Names are not unique and prove nothing: two peers may share one, and each is still
/// someone else as far as undo goes. A caller that sends no id is known by its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Peer {
    pub id: String,
    pub name: String,
}

impl Peer {
    #[cfg(test)]
    pub fn new(id: &str, name: &str) -> Self {
        Self { id: id.to_owned(), name: name.to_owned() }
    }
}

impl From<&str> for Peer {
    fn from(name: &str) -> Self {
        Self { id: format!("name:{name}"), name: name.to_owned() }
    }
}

/// One change-log entry, as the JSON string stored in the document.
fn log_entry(peer: &Peer, kind: &str, label: Option<&str>, ops: serde_json::Value) -> String {
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    serde_json::json!({ "peer": peer.id, "actor": peer.name, "kind": kind, "label": label, "ops": ops, "at": at }).to_string()
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

    /// The room for a graph, loading it from storage if this is its first use. `viewer_key` is
    /// the graph's viewer key, which unseals what is stored; the room keeps only what it derives.
    pub fn get(&self, id: &str, viewer_key: &str) -> Result<Arc<Room>, String> {
        let mut rooms = self.rooms.lock().expect("rooms lock");
        if let Some(room) = rooms.get(id) {
            return Ok(room.clone());
        }
        let room = Room::open(id, self.store.clone(), Cipher::for_viewer_key(viewer_key))?;
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

    /// Stands in for a graph's viewer key.
    const KEY: &str = "wp_viewer_test";

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
        let room = rooms.get("g1", KEY).unwrap();
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
        assert_eq!(rooms_at(&db).get("g1", KEY).unwrap().graph.snapshot().title, "Launch plan");
    }

    #[test]
    fn graph_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");

        let rooms = rooms_at(&db);
        let room = rooms.get("g1", KEY).unwrap();
        add_nodes(&room, 3);
        let before = room.graph.places();
        rooms.store.flush();
        drop((room, rooms));

        let reopened = rooms_at(&db).get("g1", KEY).unwrap();
        assert_eq!(reopened.graph.places(), before);
        assert_eq!(before.len(), 3);
    }

    #[test]
    fn graphs_are_independent() {
        let dir = tempfile::tempdir().unwrap();
        let rooms = rooms_at(&dir.path().join("w.db"));
        add_nodes(&rooms.get("a", KEY).unwrap(), 2);
        add_nodes(&rooms.get("b", KEY).unwrap(), 5);
        assert_eq!(rooms.get("a", KEY).unwrap().graph.places().len(), 2);
        assert_eq!(rooms.get("b", KEY).unwrap().graph.places().len(), 5);
        rooms.store.flush();
        assert_eq!(rooms.store.graph_ids().unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn demo_is_seeded_once_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");
        for _ in 0..3 {
            let rooms = rooms_at(&db);
            assert_eq!(rooms.get("demo", KEY).unwrap().graph.places().len(), 3);
            rooms.store.flush();
        }
    }

    #[test]
    fn compaction_shrinks_the_log_and_loses_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");

        let rooms = rooms_at(&db);
        let room = rooms.get("big", KEY).unwrap();
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

        let reopened = rooms_at(&db).get("big", KEY).unwrap();
        assert_eq!(reopened.graph.places(), expected);
        assert_eq!(expected.len(), COMPACT_AFTER as usize + 60);
    }

    /// Every byte of the database file and its log, as one buffer.
    fn raw_bytes(db: &std::path::Path) -> Vec<u8> {
        let wal = std::path::PathBuf::from(format!("{}-wal", db.display()));
        [std::fs::read(db).unwrap(), std::fs::read(wal).unwrap_or_default()].concat()
    }

    fn contains(haystack: &[u8], needle: &str) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle.as_bytes())
    }

    #[test]
    fn a_copy_of_the_database_holds_no_readable_graph_and_needs_the_key() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");
        let rooms = rooms_at(&db);
        let room = rooms.get("g1", KEY).unwrap();
        room.graph.create_node(NodeId::new(), "launch codes", Place::at_origin(Vec2::new(0, 0)));
        room.set_title("owner", "Quarterly secrets").unwrap();
        rooms.store.flush();
        assert!(!contains(&raw_bytes(&db), "launch codes"), "an update is stored in the clear");
        assert!(!contains(&raw_bytes(&db), "Quarterly secrets"));

        // After compaction the snapshot is sealed as well.
        add_nodes(&room, COMPACT_AFTER as usize);
        rooms.store.flush();
        room.maybe_compact();
        rooms.store.flush();
        assert!(rooms.store.load("g1").unwrap().snapshot.is_some_and(|s| s.sealed));
        assert!(!contains(&raw_bytes(&db), "launch codes"), "the snapshot is stored in the clear");
        drop((room, rooms));

        assert!(rooms_at(&db).get("g1", "wp_viewer_other").is_err(), "another key must not open it");
        let reopened = rooms_at(&db).get("g1", KEY).unwrap();
        assert_eq!(reopened.graph.snapshot().title, "Quarterly secrets");
    }

    #[test]
    fn plaintext_from_before_encryption_is_sealed_when_the_graph_is_next_opened() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");
        // A database as the previous version left it: no `enc` column, plaintext rows.
        let old = Graph::new();
        old.create_node(NodeId::new(), "old plaintext", Place::at_origin(Vec2::new(0, 0)));
        let update = old.encode_state();
        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(
                "CREATE TABLE graphs (id TEXT PRIMARY KEY, created_at INTEGER NOT NULL DEFAULT (unixepoch()));
                 CREATE TABLE updates (seq INTEGER PRIMARY KEY AUTOINCREMENT, graph TEXT NOT NULL REFERENCES graphs(id), data BLOB NOT NULL);
                 CREATE TABLE snapshots (graph TEXT PRIMARY KEY REFERENCES graphs(id), data BLOB NOT NULL);
                 INSERT INTO graphs (id) VALUES ('g1');",
            )
            .unwrap();
            conn.execute("INSERT INTO updates (graph, data) VALUES ('g1', ?1)", [&update]).unwrap();
        }
        assert!(contains(&raw_bytes(&db), "old plaintext"));

        let rooms = rooms_at(&db);
        assert_eq!(rooms.get("g1", KEY).unwrap().graph.places().len(), 1, "old data still loads");
        rooms.store.flush();
        let stored = rooms.store.load("g1").unwrap();
        // (Opening may add updates of its own, such as numbering nodes; those are sealed too.)
        assert!(stored.snapshot.is_some_and(|s| s.sealed) && stored.updates.iter().all(|u| u.sealed));
        assert!(!contains(&raw_bytes(&db), "old plaintext"), "plaintext left in the file");
        drop(rooms);
        assert_eq!(rooms_at(&db).get("g1", KEY).unwrap().graph.places().len(), 1);
    }

    fn batch(ops: serde_json::Value) -> Batch {
        serde_json::from_value(serde_json::json!({ "ops": ops })).unwrap()
    }

    #[test]
    fn an_applied_batch_reaches_clients_gets_keys_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("w.db");
        let rooms = rooms_at(&db);
        let room = rooms.get("agent", KEY).unwrap();
        let mut client = room.relay.subscribe(); // stands in for a connected browser

        let results = room
            .apply(&batch(serde_json::json!([
                { "op": "create_node", "title": "design" },
                { "op": "create_node", "title": "build", "destinations": ["$0"] },
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
        let reopened = rooms_at(&db).get("agent", KEY).unwrap().graph.snapshot();
        assert_eq!(reopened.nodes, expected.nodes);
        assert_eq!(reopened.edges, expected.edges);
        assert_eq!(reopened.nodes.values().filter_map(|n| n.key).max(), Some(2));
    }

    #[test]
    fn each_actor_undoes_only_their_own_batches_and_the_log_says_who() {
        let dir = tempfile::tempdir().unwrap();
        let room = rooms_at(&dir.path().join("w.db")).get("undo", KEY).unwrap();
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
    fn peers_with_the_same_name_are_still_different_peers() {
        let dir = tempfile::tempdir().unwrap();
        let room = rooms_at(&dir.path().join("w.db")).get("peers", KEY).unwrap();
        let (one, two) = (Peer::new("p1", "alex"), Peer::new("p2", "alex"));
        let add = |title: &str, who: &Peer| {
            room.apply(&batch(serde_json::json!([{ "op": "create_node", "title": title }])), who.clone()).unwrap();
        };
        add("from one", &one);
        add("from two", &two);
        assert!(room.undo(one.clone(), false));
        let left: Vec<_> = room.graph.snapshot().nodes.values().map(|n| n.title.clone()).collect();
        assert_eq!(left, ["from two"], "undo touched only the peer that asked");
        assert!(!room.undo(one.clone(), false));
        let log = room.graph.log();
        assert!(log.iter().all(|e| e["actor"] == "alex"));
        assert_eq!(log.iter().map(|e| e["peer"].as_str().unwrap()).collect::<Vec<_>>(), ["p1", "p2", "p1"]);
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

    #[test]
    fn background_is_owner_set_and_edges_stay_readable() {
        let dir = tempfile::tempdir().unwrap();
        let rooms = rooms_at(&dir.path().join("w.db"));
        let room = rooms.get("g1", KEY).unwrap();
        assert!(room.set_background("owner", "blue").is_err());
        // The default edge colours are light, so a light canvas recolours them.
        let (color, changed) = room.set_background("owner", "#F0F0F0").unwrap();
        assert_eq!((color.as_str(), changed.as_slice()), ("#f0f0f0", ["link".to_owned()].as_slice()));
        let snap = room.graph.snapshot();
        assert_eq!(snap.background, "#f0f0f0");
        assert_eq!((snap.edge_types[0].color.as_str(), snap.edge_types[0].text_color.as_str()), ("#000000", "#000000"));
        // A dark canvas needs no change to dark-on-dark ... but black lines are now too dark, so they flip.
        assert_eq!(room.set_background("owner", "#202020").unwrap().1, ["link".to_owned()]);

        // A client writing the colour straight into the document is put back.
        let rogue = Graph::new();
        rogue.apply_writes(&[Write::SetBackground { color: "#ffffff".into() }]);
        room.graph.apply_update(&rogue.encode_state()).unwrap();
        room.repair();
        assert_eq!(room.graph.snapshot().background, "#202020");
    }
}
