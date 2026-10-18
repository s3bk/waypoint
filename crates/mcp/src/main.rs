//! `waypoint-mcp`: MCP server over stdio.
//!
//! Environment:
//! - `WAYPOINT_URL`   base URL of the Waypoint server (default http://127.0.0.1:3000)
//! - `WAYPOINT_KEY`   the graph's owner, editor or viewer key (optional). The key
//!   decides what the agent may do: a viewer key reads, an editor key also changes.
//!   Without it the user gives a key in the conversation and the agent calls `connect`.
//!   Keys are never stored here: they stay in the conversation, so after a restart the
//!   agent connects again. One process can hold several graphs at once (each tool takes
//!   an optional `graph`).
//! - `WAYPOINT_ACTOR` a name for this agent in the graph's change log (default `agent`).
//!
//! stdout carries only JSON-RPC messages; diagnostics go to stderr.

use std::io::{BufRead, Write};
use std::sync::Mutex;

use serde_json::{Value, json};
use waypoint_mcp::{Backend, Config, handle};

struct Conn {
    name: String,
    graph: String,
    /// `Authorization` header value.
    auth: String,
}

#[derive(Default)]
struct Conns {
    all: Vec<Conn>,
    /// Index of the most recently connected graph: the default.
    latest: Option<usize>,
    /// Index chosen by `select` for the call in progress.
    active: Option<usize>,
}

struct Http {
    base: String,
    agent: ureq::Agent,
    /// Connected graphs, and which one the current call uses.
    conns: Mutex<Conns>,
    /// `X-Waypoint-Actor` header value: the name this process gives itself.
    actor: String,
    /// `X-Waypoint-Peer` header value: this process's id, which keys its undo history. Names may repeat; this does not.
    peer: String,
}

impl Http {
    fn auth(&self) -> Result<String, String> {
        let c = self.conns.lock().unwrap();
        c.active.and_then(|i| c.all.get(i)).map(|c| c.auth.clone()).ok_or_else(|| "not connected: ask the user for a Waypoint key and call `connect`".into())
    }

    /// Turn the server's JSON error body ({"error": "..."}) into the message an agent should see.
    fn finish(result: Result<ureq::Response, ureq::Error>) -> Result<Value, String> {
        match result {
            Ok(r) => r.into_json().map_err(|e| format!("unreadable reply from the Waypoint server: {e}")),
            Err(ureq::Error::Status(code, r)) => {
                let body: Value = r.into_json().unwrap_or(Value::Null);
                let message = body["error"].as_str().unwrap_or("request failed");
                Err(match body["op_index"].as_u64() {
                    Some(i) => format!("{message} (op {i}; nothing was changed)"),
                    None if code >= 500 => format!("Waypoint server error ({code}): {message}"),
                    None => message.to_owned(),
                })
            }
            Err(e) => Err(format!("cannot reach the Waypoint server: {e}")),
        }
    }
}

impl Backend for Http {
    fn get(&self, path_and_query: &str) -> Result<Value, String> {
        let auth = self.auth()?;
        Self::finish(self.agent.get(&format!("{}{path_and_query}", self.base)).set("Authorization", &auth).call())
    }

    fn connect(&self, key: &str, name: Option<&str>) -> Result<Value, String> {
        let auth = format!("Bearer {key}");
        let session = Self::finish(self.agent.get(&format!("{}/api/session", self.base)).set("Authorization", &auth).call())
            .map_err(|e| format!("that key was not accepted: {e}"))?;
        let graph = session["graph"].as_str().unwrap_or_default().to_owned();
        let title = Self::finish(self.agent.get(&format!("{}/api/graphs/{}", self.base, waypoint_mcp::encode(&graph))).set("Authorization", &auth).call())
            .ok()
            .and_then(|g| g["title"].as_str().map(str::to_owned))
            .filter(|t| !t.is_empty());
        let mut c = self.conns.lock().unwrap();
        // The same graph again (say with a stronger key) replaces its entry and keeps its name.
        let index = match c.all.iter().position(|x| x.graph == graph) {
            Some(i) => i,
            None => {
                let base = name.map(str::to_owned).or(title.clone()).unwrap_or_else(|| graph.chars().take(8).collect());
                let mut unique = base.clone();
                let mut n = 2;
                while c.all.iter().any(|x| x.name.eq_ignore_ascii_case(&unique)) {
                    unique = format!("{base} {n}");
                    n += 1;
                }
                c.all.push(Conn { name: unique, graph: graph.clone(), auth: auth.clone() });
                c.all.len() - 1
            }
        };
        c.all[index].auth = auth;
        if let Some(n) = name {
            c.all[index].name = n.to_owned();
        }
        c.latest = Some(index);
        let names: Vec<_> = c.all.iter().map(|x| x.name.clone()).collect();
        Ok(json!({ "name": c.all[index].name, "graph": graph, "role": session["role"], "title": title, "open_graphs": names }))
    }

    fn select(&self, graph: Option<&str>) -> Result<Option<String>, String> {
        let mut c = self.conns.lock().unwrap();
        let found = match graph {
            None => c.latest,
            Some(g) => {
                let hits: Vec<usize> = (0..c.all.len()).filter(|&i| c.all[i].name.eq_ignore_ascii_case(g) || c.all[i].graph == g).collect();
                match hits[..] {
                    [i] => Some(i),
                    _ => return Err(format!("no connected graph called \"{g}\"; open: {}", c.all.iter().map(|x| x.name.as_str()).collect::<Vec<_>>().join(", "))),
                }
            }
        };
        c.active = found;
        match found {
            Some(i) => Ok(Some(c.all[i].graph.clone())),
            None => Err("not connected: ask the user for a Waypoint key and call `connect`".into()),
        }
    }

    fn get_text(&self, path_and_query: &str) -> Result<Value, String> {
        let auth = self.auth()?;
        match self.agent.get(&format!("{}{path_and_query}", self.base)).set("Authorization", &auth).call() {
            Ok(r) => r.into_string().map(Value::String).map_err(|e| format!("unreadable reply from the Waypoint server: {e}")),
            Err(e) => Self::finish(Err(e)),
        }
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        let auth = self.auth()?;
        Self::finish(self.agent.post(&format!("{}{path}", self.base)).set("Authorization", &auth).set("X-Waypoint-Actor", &self.actor).set("X-Waypoint-Peer", &self.peer).send_json(body))
    }
}

fn main() {
    let base = std::env::var("WAYPOINT_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
    let key = std::env::var("WAYPOINT_KEY").ok().filter(|k| !k.trim().is_empty());
    let backend = Http {
        base: base.trim_end_matches('/').to_owned(),
        agent: ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(30)).build(),
        conns: Mutex::new(Conns::default()),
        actor: std::env::var("WAYPOINT_ACTOR").unwrap_or_else(|_| "agent".into()),
        peer: {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            format!("mcp-{:x}-{nanos:x}", std::process::id())
        },
    };
    // A key from the environment opens its graph at once; otherwise the agent calls `connect`.
    match key {
        Some(k) => match backend.connect(k.trim(), None) {
            Ok(s) => eprintln!("waypoint-mcp: {} key, on {}", s["role"].as_str().unwrap_or("?"), backend.base),
            Err(e) => {
                eprintln!("waypoint-mcp: {e}");
                std::process::exit(1);
            }
        },
        None => eprintln!("waypoint-mcp: no WAYPOINT_KEY; waiting for the `connect` tool"),
    }
    let cfg = Config { graph: String::new() };

    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(request) => handle(&request, &cfg, &backend),
            Err(e) => Some(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } })),
        };
        if let Some(reply) = reply {
            let _ = writeln!(out, "{reply}");
            let _ = out.flush();
        }
    }
}
