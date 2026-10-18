//! Harness: a throwaway waypoint-server (own port and database), the Vite dev
//! server proxied to it, chromedriver, and headless Chrome through WebDriver.

#![allow(dead_code)]

use std::future::Future;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Once;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use thirtyfour::prelude::*;

pub fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("workspace root")
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("free port").local_addr().expect("addr").port()
}

/// Poll `f` until it is true; fail the test after `secs`.
pub async fn until<F, Fut>(what: &str, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let end = Instant::now() + Duration::from_secs(15);
    while !f().await {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

pub struct Keys {
    pub owner: String,
    pub editor: String,
    pub viewer: String,
}

pub struct Stack {
    pub api: String,
    pub web: String,
    driver_url: String,
    children: Vec<Child>,
    _dir: tempfile::TempDir,
}

fn spawn(cmd: &mut Command) -> Child {
    cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("spawn")
}

fn wait_http(url: &str) {
    let end = Instant::now() + Duration::from_secs(30);
    while ureq::get(url).call().is_err() {
        assert!(Instant::now() < end, "timed out waiting for {url}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

impl Stack {
    pub fn start() -> Stack {
        static BUILD: Once = Once::new();
        BUILD.call_once(|| {
            let ok = Command::new("cargo")
                .args(["build", "-p", "waypoint-server", "-p", "waypoint-mcp"])
                .current_dir(workspace())
                .status()
                .expect("cargo")
                .success();
            assert!(ok, "building the server and MCP binaries failed");
        });
        let dir = tempfile::tempdir().expect("tempdir");
        let (api_port, web_port, driver_port) = (free_port(), free_port(), free_port());
        let root = workspace();
        let mut children = vec![];
        children.push(spawn(
            Command::new(root.join("target/debug/waypoint-server"))
                .env("WAYPOINT_ADDR", format!("127.0.0.1:{api_port}"))
                .env("WAYPOINT_DB", dir.path().join("e2e.db")),
        ));
        children.push(spawn(
            Command::new(root.join("client/node_modules/.bin/vite"))
                .args(["--port", &web_port.to_string(), "--strictPort", "--host", "127.0.0.1"])
                .current_dir(root.join("client"))
                .env("WAYPOINT_SERVER", format!("127.0.0.1:{api_port}")),
        ));
        children.push(spawn(Command::new("chromedriver").arg(format!("--port={driver_port}"))));
        let api = format!("http://127.0.0.1:{api_port}");
        let web = format!("http://127.0.0.1:{web_port}");
        wait_http(&format!("{api}/health"));
        wait_http(&web);
        wait_http(&format!("http://127.0.0.1:{driver_port}/status"));
        Stack { api, web, driver_url: format!("http://127.0.0.1:{driver_port}"), children, _dir: dir }
    }

    pub fn new_graph(&self) -> Keys {
        let v: Value = ureq::post(&format!("{}/api/graphs", self.api)).call().expect("new graph").into_json().expect("json");
        Keys {
            owner: v["owner"].as_str().expect("owner").into(),
            editor: v["editor"].as_str().expect("editor").into(),
            viewer: v["viewer"].as_str().expect("viewer").into(),
        }
    }

    /// Call the agent API of the graph with `key`; returns the status and JSON body.
    pub fn call(&self, keys: &Keys, key: &str, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let req = ureq::request(method, &format!("{}/api/graphs/{}{path}", self.api, keys.viewer)).set("Authorization", &format!("Bearer {key}"));
        let result = match body {
            Some(b) => req.send_json(b),
            None => req.call(),
        };
        match result {
            Ok(r) => (r.status(), r.into_json().unwrap_or(Value::Null)),
            Err(ureq::Error::Status(code, r)) => (code, r.into_json().unwrap_or(Value::Null)),
            Err(e) => panic!("cannot reach the server: {e}"),
        }
    }

    pub fn node_count(&self, keys: &Keys) -> u64 {
        self.call(keys, &keys.viewer, "GET", "/nodes", None).1["total"].as_u64().unwrap_or(0)
    }

    /// A browser tab on the graph, signed in with `key`.
    pub async fn page(&self, key: &str) -> Page {
        let mut caps = DesiredCapabilities::chrome();
        for arg in ["--headless=new", "--no-sandbox", "--window-size=1200,800", "--hide-scrollbars"] {
            caps.add_arg(arg).expect("arg");
        }
        let driver = WebDriver::new(&self.driver_url, caps).await.expect("start a browser session");
        driver.goto(format!("{}/#key={key}", self.web)).await.expect("open the client");
        let page = Page { driver };
        until("the client to load", || async { page.js("return !!window.__waypoint").await == json!(true) }).await;
        page
    }
}

impl Drop for Stack {
    fn drop(&mut self) {
        for c in &mut self.children {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Rect {
    pub fn center(&self) -> (i64, i64) {
        (self.x + self.w / 2, self.y + self.h / 2)
    }
}

pub struct Page {
    pub driver: WebDriver,
}

impl Page {
    /// Run a script in the page; `return` its result.
    pub async fn js(&self, script: &str) -> Value {
        self.driver.execute(script, vec![]).await.expect("script").json().clone()
    }

    pub async fn role_label(&self) -> String {
        self.js("return document.getElementById('share-button').textContent").await.as_str().unwrap_or_default().to_owned()
    }

    pub async fn node_ids(&self) -> Vec<String> {
        serde_json::from_value(self.js("return window.__waypoint.nodeIds()").await).expect("ids")
    }

    pub async fn title(&self, id: &str) -> String {
        self.js(&format!("return window.__waypoint.graph.title('{id}')")).await.as_str().unwrap_or_default().to_owned()
    }

    pub async fn id_titled(&self, title: &str) -> Option<String> {
        for id in self.node_ids().await {
            if self.title(&id).await == title {
                return Some(id);
            }
        }
        None
    }

    pub async fn titles(&self) -> Vec<String> {
        let mut out = vec![];
        for id in self.node_ids().await {
            out.push(self.title(&id).await);
        }
        out.sort();
        out
    }

    pub async fn edge_count(&self) -> u64 {
        self.js("return window.__waypoint.graph.edges().length").await.as_u64().unwrap_or(0)
    }

    pub async fn rect(&self, id: &str) -> Rect {
        let v = self.js(&format!("return window.__waypoint.screenRect('{id}')")).await;
        let n = |k: &str| v[k].as_f64().expect("rect field").round() as i64;
        Rect { x: n("x"), y: n("y"), w: n("w"), h: n("h") }
    }

    pub async fn editing(&self) -> Option<String> {
        self.js("return window.__waypoint.state().editing").await.as_str().map(str::to_owned)
    }

    pub async fn selected_node(&self) -> Option<String> {
        self.js("return window.__waypoint.state().selectedNode").await.as_str().map(str::to_owned)
    }

    pub async fn hint(&self) -> String {
        self.js("return document.getElementById('hint').textContent").await.as_str().unwrap_or_default().to_owned()
    }

    pub async fn dblclick(&self, x: i64, y: i64) {
        self.driver.action_chain().move_to(x, y).double_click().perform().await.expect("double click");
    }

    pub async fn click(&self, x: i64, y: i64) {
        self.driver.action_chain().move_to(x, y).click().perform().await.expect("click");
    }

    /// Press at `from`, move to `to` in a few steps, release.
    pub async fn drag(&self, from: (i64, i64), to: (i64, i64)) {
        let mut chain = self.driver.action_chain().move_to(from.0, from.1).click_and_hold();
        for i in 1..=6 {
            chain = chain.move_to(from.0 + (to.0 - from.0) * i / 6, from.1 + (to.1 - from.1) * i / 6);
        }
        chain.release().perform().await.expect("drag");
    }

    /// Type into the title box that is open, and press Enter.
    pub async fn type_title(&self, text: &str) {
        until("the title box", || async { self.editing().await.is_some() }).await;
        let editor = self.driver.find(By::Id("editor")).await.expect("editor");
        editor.send_keys(format!("{text}{}", char::from(Key::Enter))).await.expect("type");
        until("the title box to close", || async { self.editing().await.is_none() }).await;
    }

    pub async fn press(&self, key: Key) {
        let key = char::from(key);
        self.driver.action_chain().key_down(key).key_up(key).perform().await.expect("key");
    }

    pub async fn press_ctrl(&self, key: Key) {
        self.press_ctrl_char(char::from(key)).await;
    }

    pub async fn press_ctrl_char(&self, key: char) {
        self.driver
            .action_chain()
            .key_down(Key::Control)
            .key_down(key)
            .key_up(key)
            .key_up(Key::Control)
            .perform()
            .await
            .expect("chord");
    }
}
