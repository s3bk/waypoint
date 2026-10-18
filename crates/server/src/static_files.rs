//! Serve the built client (`client/dist`) when `WAYPOINT_STATIC` names a
//! directory, so one process is the whole site. Unknown paths get `index.html`.

use std::path::{Component, Path, PathBuf};

use axum::{
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
};

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "wasm" => "application/wasm",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The file for a request path inside `root`, or `None` if it escapes the root.
fn resolve(root: &Path, request: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for part in Path::new(request.trim_start_matches('/')).components() {
        match part {
            Component::Normal(p) => out.push(p),
            Component::CurDir => {}
            _ => return None, // `..` and absolute parts
        }
    }
    Some(out)
}

pub async fn serve(root: PathBuf, uri: Uri) -> Response {
    let wanted = resolve(&root, uri.path()).filter(|p| p.is_file()).unwrap_or_else(|| root.join("index.html"));
    match tokio::fs::read(&wanted).await {
        Ok(bytes) => {
            // Hashed assets never change; the page itself must be re-fetched.
            let cache = if wanted.starts_with(root.join("assets")) { "public, max-age=31536000, immutable" } else { "no-cache" };
            ([(header::CONTENT_TYPE, content_type(&wanted)), (header::CACHE_CONTROL, cache)], bytes).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_cannot_leave_the_root() {
        let root = Path::new("/srv/dist");
        assert_eq!(resolve(root, "/assets/a.js"), Some(PathBuf::from("/srv/dist/assets/a.js")));
        assert_eq!(resolve(root, "/"), Some(PathBuf::from("/srv/dist")));
        assert_eq!(resolve(root, "/../etc/passwd"), None);
        assert_eq!(resolve(root, "/a/../../x"), None);
    }
}
