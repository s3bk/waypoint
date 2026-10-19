//! SQLite persistence. Per graph, the Yjs document is stored as an
//! append-only log of updates plus an occasional snapshot (DATA_MODEL.md,
//! "Storage"). Loading applies the snapshot, then the updates after it.
//!
//! Writes go through one dedicated thread, so a document change never waits
//! on the disk. Reads use a separate connection (SQLite's WAL mode lets them
//! proceed alongside the writer).

use std::{
    path::Path,
    sync::{
        Mutex,
        mpsc::{self, Sender},
    },
    thread::{self, JoinHandle},
};

use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS graphs (
        id         TEXT PRIMARY KEY,
        created_at INTEGER NOT NULL DEFAULT (unixepoch())
    );
    CREATE TABLE IF NOT EXISTS updates (
        seq   INTEGER PRIMARY KEY AUTOINCREMENT,
        graph TEXT NOT NULL REFERENCES graphs(id),
        data  BLOB NOT NULL,
        enc   INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX IF NOT EXISTS updates_by_graph ON updates (graph, seq);
    CREATE TABLE IF NOT EXISTS snapshots (
        graph TEXT PRIMARY KEY REFERENCES graphs(id),
        data  BLOB NOT NULL,
        enc   INTEGER NOT NULL DEFAULT 0
    );
";

/// Databases made before encryption lack the `enc` column; their rows count as plaintext (0).
fn add_enc_column(conn: &Connection) -> rusqlite::Result<()> {
    for table in ["updates", "snapshots"] {
        let has: bool = conn.query_row("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = 'enc'", [table], |r| r.get::<_, i64>(0))? > 0;
        if !has {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN enc INTEGER NOT NULL DEFAULT 0"))?;
        }
    }
    Ok(())
}

enum Cmd {
    Append { graph: String, data: Vec<u8> },
    /// Replace the snapshot with `state` and drop updates up to `up_to_seq`.
    Snapshot { graph: String, up_to_seq: i64, state: Vec<u8> },
    /// Reply once everything before this command has reached the disk.
    Flush(Sender<()>),
    /// Rewrite the file and truncate the log, so plaintext that was deleted no longer lingers in free pages.
    Scrub,
}

/// One stored blob. `sealed` is false for rows from before encryption.
pub struct Blob {
    pub data: Vec<u8>,
    pub sealed: bool,
}

/// What is stored for one graph, in the order it must be applied.
#[derive(Default)]
pub struct Stored {
    pub snapshot: Option<Blob>,
    pub updates: Vec<Blob>,
    /// Highest update `seq` included in `updates` (0 if none).
    pub last_seq: i64,
}

pub struct Store {
    writer: Sender<Cmd>,
    reader: Mutex<Connection>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn open(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "secure_delete", "ON")?; // deleted rows are overwritten, not just unlinked
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let write_conn = open(path)?;
        write_conn.execute_batch(SCHEMA)?;
        add_enc_column(&write_conn)?;
        let reader = open(path)?;

        let (tx, rx) = mpsc::channel::<Cmd>();
        let thread = thread::Builder::new()
            .name("waypoint-store".into())
            .spawn(move || {
                for cmd in rx {
                    if let Err(e) = run(&write_conn, cmd) {
                        eprintln!("store: write failed: {e}");
                    }
                }
            })
            .expect("spawn store thread");
        Ok(Self { writer: tx, reader: Mutex::new(reader), thread: Mutex::new(Some(thread)) })
    }

    /// Queue an update for writing; `data` is already sealed. Returns immediately.
    pub fn append(&self, graph: &str, data: Vec<u8>) {
        let _ = self.writer.send(Cmd::Append { graph: graph.to_owned(), data });
    }

    /// Queue a snapshot (already sealed) that replaces all updates up to `up_to_seq`.
    pub fn snapshot(&self, graph: &str, up_to_seq: i64, state: Vec<u8>) {
        let _ = self.writer.send(Cmd::Snapshot { graph: graph.to_owned(), up_to_seq, state });
    }

    /// Queue a rewrite that purges deleted plaintext from the file.
    pub fn scrub(&self) {
        let _ = self.writer.send(Cmd::Scrub);
    }

    /// Block until everything queued so far is on disk.
    pub fn flush(&self) {
        let (tx, rx) = mpsc::channel();
        if self.writer.send(Cmd::Flush(tx)).is_ok() {
            let _ = rx.recv();
        }
    }

    pub fn load(&self, graph: &str) -> rusqlite::Result<Stored> {
        let conn = self.reader.lock().expect("reader lock");
        let snapshot = conn
            .query_row("SELECT data, enc FROM snapshots WHERE graph = ?1", [graph], |r| Ok(Blob { data: r.get(0)?, sealed: r.get::<_, i64>(1)? != 0 }))
            .optional()?;
        let mut stmt = conn.prepare("SELECT seq, data, enc FROM updates WHERE graph = ?1 ORDER BY seq")?;
        let mut stored = Stored { snapshot, ..Stored::default() };
        let rows = stmt.query_map([graph], |r| Ok((r.get::<_, i64>(0)?, Blob { data: r.get(1)?, sealed: r.get::<_, i64>(2)? != 0 })))?;
        for row in rows {
            let (seq, blob) = row?;
            stored.last_seq = seq;
            stored.updates.push(blob);
        }
        Ok(stored)
    }

    /// Highest update `seq` on disk for a graph (0 if none). Read this
    /// *before* encoding the state you pass to [`Store::snapshot`]: every
    /// update on disk was applied to the document first, so the state is
    /// guaranteed to contain it.
    pub fn last_seq(&self, graph: &str) -> rusqlite::Result<i64> {
        let conn = self.reader.lock().expect("reader lock");
        conn.query_row("SELECT COALESCE(MAX(seq), 0) FROM updates WHERE graph = ?1", [graph], |r| r.get(0))
    }

    pub fn has_graph(&self, graph: &str) -> bool {
        let conn = self.reader.lock().expect("reader lock");
        conn.query_row("SELECT 1 FROM graphs WHERE id = ?1", [graph], |_| Ok(())).optional().ok().flatten().is_some()
    }

    #[cfg(test)]
    pub fn graph_ids(&self) -> rusqlite::Result<Vec<String>> {
        let conn = self.reader.lock().expect("reader lock");
        let mut stmt = conn.prepare("SELECT id FROM graphs ORDER BY created_at, id")?;
        stmt.query_map([], |r| r.get(0))?.collect()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        // Closing the channel ends the writer thread after it drains the queue.
        let (dead, _) = mpsc::channel();
        self.writer = dead;
        if let Some(t) = self.thread.lock().expect("thread lock").take() {
            let _ = t.join();
        }
    }
}

fn run(conn: &Connection, cmd: Cmd) -> rusqlite::Result<()> {
    match cmd {
        Cmd::Append { graph, data } => {
            conn.execute("INSERT OR IGNORE INTO graphs (id) VALUES (?1)", [&graph])?;
            conn.execute("INSERT INTO updates (graph, data, enc) VALUES (?1, ?2, 1)", params![graph, data])?;
        }
        Cmd::Snapshot { graph, up_to_seq, state } => {
            let tx = conn.unchecked_transaction()?;
            tx.execute("INSERT OR IGNORE INTO graphs (id) VALUES (?1)", [&graph])?;
            tx.execute("INSERT OR REPLACE INTO snapshots (graph, data, enc) VALUES (?1, ?2, 1)", params![graph, state])?;
            tx.execute("DELETE FROM updates WHERE graph = ?1 AND seq <= ?2", params![graph, up_to_seq])?;
            tx.commit()?;
        }
        Cmd::Scrub => {
            conn.execute_batch("VACUUM")?;
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
        }
        Cmd::Flush(done) => {
            let _ = done.send(());
        }
    }
    Ok(())
}
