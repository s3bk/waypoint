mod api;
mod auth;
mod graph;
mod ops;
mod repair;
mod rooms;
mod static_files;
mod store;
mod sync;

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    Json, Router,
    extract::{Path, Query, State, WebSocketUpgrade},
    extract::Request,
    http::{HeaderMap, HeaderValue, StatusCode, header::{AUTHORIZATION, CONTENT_TYPE}},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use waypoint_model::Batch;
use auth::Role;
use rooms::Rooms;
use store::Store;

#[derive(Clone)]
struct App {
    rooms: Arc<Rooms>,
    next_conn: Arc<AtomicU64>,
}

#[tokio::main]
async fn main() {
    let db = PathBuf::from(std::env::var("WAYPOINT_DB").unwrap_or_else(|_| "waypoint.db".into()));
    let store = Arc::new(Store::open(&db).expect("open database"));
    println!("database: {}", db.display());

    let app = App { rooms: Arc::new(Rooms::new(store.clone())), next_conn: Arc::new(AtomicU64::new(1)) };
    let router = Router::new()
        .route("/llms.txt", get(|| async { ([(CONTENT_TYPE, "text/plain; charset=utf-8")], include_str!("../docs/llms.txt")) }))
        .route("/openapi.json", get(openapi))
        .route("/api/schema", get(openapi))
        .route("/health", get(|| async { "ok" }))
        // Anyone can ask for a new set of keys; a graph exists once an editor first writes to it.
        .route("/api/graphs", post(new_graph))
        .route("/api/graphs/{graph}", get(graph_summary))
        .route("/api/session", get(session))
        .route("/api/graphs/{graph}/positions", get(positions))
        // The agent API (see crates/mcp): read the graph, and change it in atomic batches.
        .route("/api/graphs/{graph}/nodes", get(nodes_search))
        .route("/api/graphs/{graph}/nodes/{node}", get(node_detail))
        .route("/api/graphs/{graph}/nodes/{node}/neighbours", get(node_neighbours))
        .route("/api/graphs/{graph}/conflicts", get(conflicts))
        .route("/api/graphs/{graph}/dot", get(dot))
        .route("/api/graphs/{graph}/apply", post(apply_batch))
        .route("/api/graphs/{graph}/undo", post(undo))
        .route("/api/graphs/{graph}/redo", post(redo))
        .route("/api/graphs/{graph}/log", get(change_log))
        .route("/api/graphs/{graph}/title", put(set_title))
        .route("/ws/{graph}", get(ws));
    // WAYPOINT_STATIC=client/dist makes this process serve the web client too.
    let router = match std::env::var("WAYPOINT_STATIC") {
        Ok(dir) => {
            println!("serving the client from {dir}");
            let root = PathBuf::from(dir);
            router.fallback(move |uri| static_files::serve(root.clone(), uri))
        }
        Err(_) => router,
    }
    .layer(middleware::from_fn(key_from_query))
    .with_state(app);
    let addr = std::env::var("WAYPOINT_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let listener = tokio::net::TcpListener::bind(&addr).await.expect("bind");
    println!("waypoint-server listening on http://{addr}");
    // Ctrl-C stops accepting connections and lets open ones finish, but a
    // WebSocket client can stay connected indefinitely: give it two seconds.
    let shutdown_store = store.clone();
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            // Ctrl-C, or SIGTERM from a service manager.
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                shutdown_store.flush();
                std::process::exit(0);
            });
        })
        .await
        .expect("serve");
    // Let the writer thread drain its queue before exiting.
    store.flush();
}

/// Fresh keys for a new graph. Nothing is stored until someone writes with them.
async fn new_graph() -> Json<Value> {
    Json(auth::Keys::generate().to_json())
}

/// What the presented key may do, which graph it opens (named by its viewer
/// key, see auth.rs) and the keys it can hand out.
async fn session(State(app): State<App>, headers: HeaderMap) -> Result<Json<Value>, Response> {
    let key = presented_key(&headers, None, "");
    let (_, graph) = auth::claim(&key).ok_or_else(unauthorized)?;
    let role = auth::role_for(&key, &graph).ok_or_else(unauthorized)?;
    let exists = app.rooms.exists(&auth::storage_id(&graph));
    Ok(Json(json!({ "role": role.as_str(), "graph": graph, "exists": exists, "keys": auth::shareable(&key, role) })))
}

/// Derived absolute positions of every live node.
async fn positions(State(app): State<App>, headers: HeaderMap, Path(graph): Path<String>) -> Result<Json<serde_json::Value>, Response> {
    let (room, _) = access(&app, &headers, &graph, None, Role::Viewer)?;
    let pos = waypoint_layout::derive_positions(&room.graph.places());
    Ok(Json(serde_json::to_value(pos).expect("positions serialize")))
}

async fn openapi() -> impl IntoResponse {
    ([(CONTENT_TYPE, "application/json")], include_str!("../docs/openapi.json"))
}

/// Every error body points at the docs, so a caller that tried the API blind finds them.
fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into(), "docs": "/llms.txt" }))).into_response()
}

async fn graph_summary(State(app): State<App>, headers: HeaderMap, Path(graph): Path<String>) -> Result<Json<Value>, Response> {
    let (room, _) = access(&app, &headers, &graph, None, Role::Viewer)?;
    Ok(Json(api::summary(&room.graph.snapshot())))
}

async fn conflicts(State(app): State<App>, headers: HeaderMap, Path(graph): Path<String>) -> Result<Json<Value>, Response> {
    Ok(Json(api::conflicts(&access(&app, &headers, &graph, None, Role::Viewer)?.0.graph.snapshot())))
}

#[derive(Deserialize)]
struct TitleBody {
    title: String,
}

/// Name the graph. Only the owner key may; the title is shown to everyone who opens it.
async fn set_title(
    State(app): State<App>,
    headers: HeaderMap,
    Path(graph): Path<String>,
    Json(body): Json<TitleBody>,
) -> Result<Json<Value>, Response> {
    let (room, _) = access(&app, &headers, &graph, None, Role::Owner)?;
    let title = room.set_title(&actor(&headers), &body.title).map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    Ok(Json(json!({ "title": title })))
}

/// The graph as Graphviz DOT text.
async fn dot(State(app): State<App>, headers: HeaderMap, Path(graph): Path<String>) -> Result<Response, Response> {
    let (room, _) = access(&app, &headers, &graph, None, Role::Viewer)?;
    let text = api::dot(&room.graph.snapshot());
    Ok(([(axum::http::header::CONTENT_TYPE, "text/vnd.graphviz; charset=utf-8")], text).into_response())
}

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
    limit: Option<usize>,
}

async fn nodes_search(
    State(app): State<App>,
    headers: HeaderMap,
    Path(graph): Path<String>,
    Query(q): Query<SearchQuery>,
) -> Result<Json<Value>, Response> {
    let (room, _) = access(&app, &headers, &graph, None, Role::Viewer)?;
    Ok(Json(api::search(&room.graph.snapshot(), q.q.as_deref(), q.limit.unwrap_or(50).clamp(1, 500))))
}

async fn node_detail(
    State(app): State<App>,
    headers: HeaderMap,
    Path((graph, node)): Path<(String, String)>,
) -> Result<Json<Value>, Response> {
    let (room, _) = access(&app, &headers, &graph, None, Role::Viewer)?;
    api::node(&room.graph.snapshot(), &node).map(Json).map_err(|e| error(StatusCode::NOT_FOUND, e))
}

#[derive(Deserialize)]
struct NeighbourQuery {
    direction: Option<String>,
    depth: Option<usize>,
}

async fn node_neighbours(
    State(app): State<App>,
    headers: HeaderMap,
    Path((graph, node)): Path<(String, String)>,
    Query(q): Query<NeighbourQuery>,
) -> Result<Json<Value>, Response> {
    let (room, _) = access(&app, &headers, &graph, None, Role::Viewer)?;
    api::neighbours(&room.graph.snapshot(), &node, q.direction.as_deref().unwrap_or("both"), q.depth.unwrap_or(1))
        .map(Json)
        .map_err(|e| error(StatusCode::BAD_REQUEST, e))
}

/// Who is acting, from `X-Waypoint-Actor`: a label the caller chooses, not proof of identity.
fn actor(headers: &HeaderMap) -> String {
    let name: String = headers
        .get("x-waypoint-actor")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        .take(40)
        .collect();
    if name.trim().is_empty() { "anonymous".into() } else { name }
}

async fn undo(State(app): State<App>, headers: HeaderMap, Path(graph): Path<String>) -> Result<Json<Value>, Response> {
    step(app, headers, graph, false)
}

async fn redo(State(app): State<App>, headers: HeaderMap, Path(graph): Path<String>) -> Result<Json<Value>, Response> {
    step(app, headers, graph, true)
}

/// Undo or redo the caller's own latest batch; 409 if there is none.
fn step(app: App, headers: HeaderMap, graph: String, redo: bool) -> Result<Json<Value>, Response> {
    let (room, _) = access(&app, &headers, &graph, None, Role::Editor)?;
    if room.undo(&actor(&headers), redo) {
        Ok(Json(json!({ "ok": true })))
    } else {
        Err(error(StatusCode::CONFLICT, format!("nothing to {}", if redo { "redo" } else { "undo" })))
    }
}

/// Who changed what, oldest first.
async fn change_log(State(app): State<App>, headers: HeaderMap, Path(graph): Path<String>) -> Result<Json<Value>, Response> {
    let (room, _) = access(&app, &headers, &graph, None, Role::Viewer)?;
    Ok(Json(json!({ "log": room.graph.log() })))
}

/// Apply a batch of operations atomically. On failure nothing changes and the
/// response names the op that failed.
async fn apply_batch(
    State(app): State<App>,
    headers: HeaderMap,
    Path(graph): Path<String>,
    Json(batch): Json<Batch>,
) -> Result<Json<Value>, Response> {
    let (room, _) = access(&app, &headers, &graph, None, Role::Editor)?;
    match room.apply(&batch, &actor(&headers)) {
        Ok(results) => Ok(Json(json!({ "results": results }))),
        Err(e) => Err((StatusCode::UNPROCESSABLE_ENTITY, Json(json!({ "error": e.message, "op_index": e.op_index, "docs": "/llms.txt" }))).into_response()),
    }
}

#[derive(Deserialize)]
struct WsQuery {
    key: Option<String>,
}

/// Browsers cannot set headers on a WebSocket, so the key may come as `?key=`.
/// A viewer may connect and watch; its edits are ignored.
async fn ws(
    upgrade: WebSocketUpgrade,
    State(app): State<App>,
    headers: HeaderMap,
    Path(graph): Path<String>,
    Query(q): Query<WsQuery>,
) -> Response {
    let (room, role) = match access(&app, &headers, &graph, q.key.as_deref(), Role::Viewer) {
        Ok(found) => found,
        Err(response) => return response,
    };
    let conn = app.next_conn.fetch_add(1, Ordering::Relaxed);
    upgrade.on_upgrade(move |socket| sync::serve(socket, room, conn, role >= Role::Editor))
}

/// Let any route take the key as `?key=` when the request has no Authorization
/// header, for callers that cannot set headers. Keys in URLs can end up in logs
/// and browser history, so a header is still preferred.
async fn key_from_query(mut req: Request, next: Next) -> Response {
    if !req.headers().contains_key(AUTHORIZATION) {
        let key = req.uri().query().and_then(|q| {
            q.split('&').find_map(|pair| pair.strip_prefix("key="))
        });
        if let Some(v) = key.and_then(|k| HeaderValue::from_str(&format!("Bearer {k}")).ok()) {
            req.headers_mut().insert(AUTHORIZATION, v);
        }
    }
    next.run(req).await
}

fn unauthorized() -> Response {
    error(StatusCode::UNAUTHORIZED, "missing or wrong key for this graph")
}

/// The key a request carries: a bearer token, else `key_param`, else (for
/// read-only access) the viewer key in the path itself.
fn presented_key(headers: &HeaderMap, key_param: Option<&str>, graph: &str) -> String {
    headers
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .or(key_param)
        .unwrap_or(graph)
        .trim()
        .to_owned()
}

/// Check the caller's key against the graph named in the path (its viewer key)
/// and open the graph. Wrong keys and unknown graphs get the same answer, so a
/// guess reveals nothing. A graph comes into being when an editor or owner first
/// uses it; a viewer cannot create one.
fn access(
    app: &App,
    headers: &HeaderMap,
    graph: &str,
    key_param: Option<&str>,
    need: Role,
) -> Result<(Arc<rooms::Room>, Role), Response> {
    let key = presented_key(headers, key_param, graph);
    let role = auth::role_for(&key, graph).ok_or_else(unauthorized)?;
    if role < need {
        return Err(error(StatusCode::FORBIDDEN, format!("this needs an {} key; yours is a {} key", need.as_str(), role.as_str())));
    }
    let id = auth::storage_id(graph);
    if role == Role::Viewer && !app.rooms.exists(&id) {
        return Err(unauthorized());
    }
    let room = app.rooms.get(&id).map_err(|e| {
        eprintln!("loading graph: {e}");
        StatusCode::INTERNAL_SERVER_ERROR.into_response()
    })?;
    Ok((room, role))
}
