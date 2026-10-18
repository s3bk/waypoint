//! `waypoint-mcp`: MCP server over stdio.
//!
//! Environment:
//! - `WAYPOINT_URL`   base URL of the Waypoint server (default http://127.0.0.1:3000)
//! - `WAYPOINT_KEY`   the graph's owner, editor or viewer key (required). The key
//!   decides what the agent may do: a viewer key reads, an editor key also changes.
//! - `WAYPOINT_ACTOR` a name for this agent in the graph's change log (default `agent`).
//!
//! stdout carries only JSON-RPC messages; diagnostics go to stderr.

use std::io::{BufRead, Write};

use serde_json::{Value, json};
use waypoint_mcp::{Backend, Config, handle};

struct Http {
    base: String,
    agent: ureq::Agent,
    /// `Authorization` header value.
    auth: String,
    /// `X-Waypoint-Actor` header value: the name this process gives itself.
    actor: String,
    /// `X-Waypoint-Peer` header value: this process's id, which keys its undo history. Names may repeat; this does not.
    peer: String,
}

impl Http {
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
        Self::finish(self.agent.get(&format!("{}{path_and_query}", self.base)).set("Authorization", &self.auth).call())
    }

    fn get_text(&self, path_and_query: &str) -> Result<Value, String> {
        match self.agent.get(&format!("{}{path_and_query}", self.base)).set("Authorization", &self.auth).call() {
            Ok(r) => r.into_string().map(Value::String).map_err(|e| format!("unreadable reply from the Waypoint server: {e}")),
            Err(e) => Self::finish(Err(e)),
        }
    }

    fn post(&self, path: &str, body: &Value) -> Result<Value, String> {
        Self::finish(self.agent.post(&format!("{}{path}", self.base)).set("Authorization", &self.auth).set("X-Waypoint-Actor", &self.actor).set("X-Waypoint-Peer", &self.peer).send_json(body))
    }
}

fn main() {
    let base = std::env::var("WAYPOINT_URL").unwrap_or_else(|_| "http://127.0.0.1:3000".into());
    let Ok(key) = std::env::var("WAYPOINT_KEY") else {
        eprintln!("waypoint-mcp: set WAYPOINT_KEY to the graph's owner, editor or viewer key");
        std::process::exit(2);
    };
    let backend = Http {
        base: base.trim_end_matches('/').to_owned(),
        agent: ureq::AgentBuilder::new().timeout(std::time::Duration::from_secs(30)).build(),
        auth: format!("Bearer {}", key.trim()),
        actor: std::env::var("WAYPOINT_ACTOR").unwrap_or_else(|_| "agent".into()),
        peer: {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            format!("mcp-{:x}-{nanos:x}", std::process::id())
        },
    };
    // The graph is named by its viewer key, which every key can derive; the
    // server does that for us and tells us the role while it is at it.
    let (graph, role) = match backend.get("/api/session") {
        Ok(s) => (s["graph"].as_str().unwrap_or_default().to_owned(), s["role"].as_str().unwrap_or("?").to_owned()),
        Err(e) => {
            eprintln!("waypoint-mcp: {e}");
            std::process::exit(1);
        }
    };
    let cfg = Config { graph };
    eprintln!("waypoint-mcp: {role} key, on {}", backend.base);

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
