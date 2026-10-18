// Which graph this tab opens and what its key allows. A key is a bearer
// credential (see crates/server/src/auth.rs): the graph is named by its viewer
// key, which the server derives from whichever key we hold.

export type Role = "owner" | "editor" | "viewer";

export interface Access {
  /** The key we hold. */
  key: string;
  role: Role;
  /** The graph's viewer key, which names it in the API. */
  graph: string;
  /** Keys this one can hand out, by role. */
  keys: Partial<Record<Role, string>>;
}

const STORED = "waypoint.key";

function load(): string | null {
  try {
    return localStorage.getItem(STORED);
  } catch {
    return null;
  }
}

function save(key: string) {
  try {
    localStorage.setItem(STORED, key);
  } catch {
    // Private windows may refuse; the key then lasts for this tab only.
  }
}

async function session(key: string): Promise<Access | null> {
  const r = await fetch("/api/session", { headers: { Authorization: `Bearer ${key}` } });
  if (!r.ok) return null;
  const s = await r.json();
  return { key, role: s.role, graph: s.graph, keys: s.keys };
}

/** A link that opens the graph with `key`. */
export function linkFor(key: string): string {
  return `${location.origin}${location.pathname}#key=${key}`;
}

export interface NewGraph {
  owner: string;
  editor: string;
  viewer: string;
}

const ROLE_RANK: Record<Role, number> = { viewer: 0, editor: 1, owner: 2 };

/** A graph this browser has opened: its name and the strongest key it has held for it. */
export interface SavedGraph {
  graph: string;
  title: string;
  key: string;
  role: Role;
  at: number;
}

const SAVED = "waypoint.graphs";

/** Graphs this browser knows, most recently opened first. Keys are stored in the clear, like any bearer link you keep. */
export function savedGraphs(): SavedGraph[] {
  try {
    const list = JSON.parse(localStorage.getItem(SAVED) ?? "[]");
    return Array.isArray(list) ? list.filter((g) => g && typeof g.key === "string" && typeof g.graph === "string") : [];
  } catch {
    return [];
  }
}

function writeSaved(list: SavedGraph[]) {
  try {
    localStorage.setItem(SAVED, JSON.stringify(list));
  } catch {
    // Private windows may refuse; nothing is remembered.
  }
}

/** Remember (title, key) for this graph. A weaker key never replaces a stronger one already kept. */
export function rememberGraph(access: Access, title: string) {
  const list = savedGraphs();
  const old = list.find((g) => g.graph === access.graph);
  const keep = old && ROLE_RANK[old.role] > ROLE_RANK[access.role];
  const entry: SavedGraph = {
    graph: access.graph,
    title: title || old?.title || "",
    key: keep ? old.key : access.key,
    role: keep ? old.role : access.role,
    at: Date.now(),
  };
  writeSaved([entry, ...list.filter((g) => g.graph !== access.graph)]);
}

export function forgetGraph(graph: string) {
  writeSaved(savedGraphs().filter((g) => g.graph !== graph));
}

/** Make a new graph on the server and have its owner name it. Its keys cannot be recovered later, so the caller must show them. */
export async function createGraph(title: string): Promise<NewGraph> {
  const r = await fetch("/api/graphs", { method: "POST" });
  if (!r.ok) throw new Error(`could not create a graph (${r.status})`);
  const made = (await r.json()) as NewGraph;
  await setGraphTitle(made.owner, title);
  return made;
}

/** Name the graph `key` opens. Only an owner key may. */
export async function setGraphTitle(key: string, title: string): Promise<string> {
  const s = await session(key);
  if (!s) throw new Error("that key was not accepted");
  const r = await fetch(`/api/graphs/${encodeURIComponent(s.graph)}/title`, {
    method: "PUT",
    headers: { Authorization: `Bearer ${key}`, "Content-Type": "application/json" },
    body: JSON.stringify({ title }),
  });
  if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error ?? `could not set the title (${r.status})`);
  return (await r.json()).title;
}

/** Ask for a name until one is given; null if the person cancels. */
export function askTitle(initial = ""): string | null {
  for (;;) {
    const answer = window.prompt("Name for this graph (shown to everyone who opens it):", initial);
    if (answer === null) return null;
    if (answer.trim()) return answer.trim();
  }
}

/** What the start dialog can end with: a key to try, or a request for a new graph. */
type Choice = { key: string } | { create: true };

/** The start dialog: graphs this browser knows, a place to paste a key, and "New graph". */
function chooseGraph(problem: string): Promise<Choice | null> {
  return new Promise((resolve) => {
    const known = savedGraphs();
    const box = document.createElement("div");
    box.id = "start";
    box.style.cssText =
      "position:fixed;inset:0;z-index:20;display:flex;align-items:center;justify-content:center;background:#14161aee;font:14px system-ui,sans-serif;color:#aab4c3";
    const card = document.createElement("div");
    card.style.cssText = "width:min(460px,92vw);max-height:90vh;overflow:auto;padding:16px;background:#1d2128;border:1px solid #2b3340;border-radius:10px";
    const done = (c: Choice | null) => {
      box.remove();
      resolve(c);
    };
    const h = document.createElement("h2");
    h.textContent = "Waypoint";
    h.style.cssText = "margin:0 0 8px;color:#e6e9ef;font-size:18px";
    card.append(h);
    if (problem) {
      const p = document.createElement("p");
      p.textContent = problem;
      p.style.color = "#ff7b7b";
      card.append(p);
    }
    for (const g of known) {
      const row = document.createElement("div");
      row.style.cssText = "display:flex;gap:8px;align-items:center;margin:6px 0";
      const open = document.createElement("button");
      open.className = "saved-graph";
      open.textContent = `${g.title || "Untitled graph"} · ${g.role}`;
      open.style.cssText = "flex:1;text-align:left;padding:8px 10px;background:#14161a;color:#e6e9ef;border:1px solid #2b3340;border-radius:6px;cursor:pointer;font:inherit";
      open.onclick = () => done({ key: g.key });
      const forget = document.createElement("button");
      forget.textContent = "Forget";
      forget.title = "Remove this graph and its key from this browser";
      forget.style.cssText = "padding:6px 8px;background:none;color:#8793a8;border:1px solid #2b3340;border-radius:6px;cursor:pointer;font:inherit";
      forget.onclick = () => {
        forgetGraph(g.graph);
        row.remove();
      };
      row.append(open, forget);
      card.append(row);
    }
    const paste = document.createElement("input");
    paste.placeholder = "Paste a key (owner, editor or viewer) and press Enter";
    paste.style.cssText = "width:100%;box-sizing:border-box;margin:10px 0;padding:8px 10px;background:#14161a;color:#e6e9ef;border:1px solid #2b3340;border-radius:6px;font:inherit";
    paste.onkeydown = (e) => {
      if (e.key === "Enter" && paste.value.trim()) done({ key: paste.value.trim() });
    };
    const fresh = document.createElement("button");
    fresh.id = "start-new";
    fresh.textContent = "New graph";
    fresh.style.cssText = "padding:8px 12px;background:#2b3340;color:#e6e9ef;border:0;border-radius:6px;cursor:pointer;font:inherit";
    fresh.onclick = () => done({ create: true });
    card.append(paste, fresh);
    box.append(card);
    document.body.append(box);
    paste.focus();
  });
}

/**
 * Find a working key: from the address (`#key=…`, which is then removed from
 * view), from the last visit, or from the start dialog (graphs this browser
 * knows, a pasted key, or a new graph).
 */
export async function getAccess(): Promise<Access> {
  const fromHash = new URLSearchParams(location.hash.slice(1)).get("key");
  if (fromHash) history.replaceState(null, "", location.pathname + location.search);
  let key = fromHash || load();
  let problem = "";
  for (;;) {
    if (key) {
      const found = await session(key);
      if (found) {
        save(key);
        return found;
      }
      problem = "That key was not accepted. ";
    }
    const choice = await chooseGraph(problem);
    if (!choice) throw new Error("no key");
    if ("key" in choice) {
      key = choice.key;
      continue;
    }
    const title = askTitle();
    if (title === null) continue;
    const made = await createGraph(title);
    key = made.owner;
    window.alert(
      `New graph "${title}". Keep these keys; they cannot be recovered.\n\nowner (everything, and can share the others):\n${made.owner}\n\neditor:\n${made.editor}\n\nviewer:\n${made.viewer}`,
    );
  }
}
