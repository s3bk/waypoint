import { Application, Container, Graphics, Text } from "pixi.js";
import { WebsocketProvider } from "y-websocket";
import init, {
  creates_cycle,
  derive_positions,
  move_node,
} from "./wasm/waypoint_layout.js";
import { askTitle, createGraph, getAccess, linkFor as shareLink, rememberGraph, setGraphTitle, type Access, type Role } from "./access";
import { Camera } from "./camera";
import { Graph, type Place, type Vec2 } from "./graph";
import { setupMargin } from "./margin";

const NEW_TITLE = "New node";

// Must match waypoint_model::NODE_WIDTH / NODE_HEIGHT.
const NODE_W = 160;
const NODE_H = 56;

const EDGE_COLOR = 0x8aa0bd;
const EDGE_SELECTED = 0xffb454;
const EDGE_CONFLICTED = 0xe5484d;
const ACCENT = 0x5aa9ff;
const SELECTED = 0xcfe3ff;
const OK = 0x4cc38a;
const BAD = 0xe5534b;
const ARROW_LEN = 10;
const ARROW_HALF_WIDTH = 5;
/** Screen-pixel sizes: handles and edge hit areas stay the same size at any zoom. */
const HANDLE_R = 6;
const HANDLE_HIT = 11;
const EDGE_HIT = 8;

/** Point where the ray from the centre of a node box toward `target` leaves the box. */
function boxExit(center: Vec2, target: Vec2): Vec2 {
  const dx = target.x - center.x;
  const dy = target.y - center.y;
  if (dx === 0 && dy === 0) return center;
  const t = Math.min(
    dx === 0 ? Infinity : NODE_W / 2 / Math.abs(dx),
    dy === 0 ? Infinity : NODE_H / 2 / Math.abs(dy),
  );
  return { x: center.x + dx * t, y: center.y + dy * t };
}

/** A line from `a` to `b` with the arrowhead tip at `b`. */
function arrow(g: Graphics, a: Vec2, b: Vec2, color: number, width = 2) {
  const angle = Math.atan2(b.y - a.y, b.x - a.x);
  // Stop the line at the arrow base so the line width does not poke through the tip.
  const base = { x: b.x - Math.cos(angle) * ARROW_LEN, y: b.y - Math.sin(angle) * ARROW_LEN };
  g.moveTo(a.x, a.y).lineTo(base.x, base.y).stroke({ width, color, cap: "round" });
  const nx = -Math.sin(angle) * ARROW_HALF_WIDTH;
  const ny = Math.cos(angle) * ARROW_HALF_WIDTH;
  g.poly([b.x, b.y, base.x + nx, base.y + ny, base.x - nx, base.y - ny]).fill(color);
}

/** Like `arrow`, but the line is dashed: a link that is kept but not in force. */
function dashedArrow(g: Graphics, a: Vec2, b: Vec2, color: number, width = 2) {
  const len = Math.hypot(b.x - a.x, b.y - a.y);
  const ux = (b.x - a.x) / (len || 1);
  const uy = (b.y - a.y) / (len || 1);
  const line = Math.max(0, len - ARROW_LEN);
  for (let d = 0; d < line; d += 14) {
    const e = Math.min(d + 8, line);
    g.moveTo(a.x + ux * d, a.y + uy * d).lineTo(a.x + ux * e, a.y + uy * e).stroke({ width, color, cap: "round" });
  }
  g.poly([b.x, b.y, b.x - ux * ARROW_LEN - uy * ARROW_HALF_WIDTH, b.y - uy * ARROW_LEN + ux * ARROW_HALF_WIDTH, b.x - ux * ARROW_LEN + uy * ARROW_HALF_WIDTH, b.y - uy * ARROW_LEN - ux * ARROW_HALF_WIDTH]).fill(color);
}

function distanceToSegment(p: Vec2, a: Vec2, b: Vec2): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const len2 = dx * dx + dy * dy;
  const t = len2 === 0 ? 0 : Math.max(0, Math.min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / len2));
  return Math.hypot(p.x - (a.x + t * dx), p.y - (a.y + t * dy));
}

/** A drag in progress, local or a collaborator's (shared over awareness, never written to the document until drop). */
interface Drag {
  node: string;
  /** Where the node should end up, absolute in the scope. */
  x: number;
  y: number;
  /** Take the node's whole anchored sub-graph along; Shift drags only the node. */
  subtree: boolean;
}

/**
 * A link being drawn from a node's side dot. The link points from the node
 * where the drag starts to the node where it stops, with the arrowhead on the
 * stop node (the start node depends on it). Either dot works the same; the
 * side only decides where the arrow leaves the node.
 */
interface Connecting {
  node: string;
  side: "left" | "right";
  /** Pointer, in world coordinates. */
  x: number;
  y: number;
  /** Where the press happened, to tell a click on the dot from a drag. */
  fromX: number;
  fromY: number;
}

const ROLE_BLURB: Record<Role, string> = {
  owner: "Can do everything, and share the editor and viewer keys.",
  editor: "Can change the graph, and share the viewer key.",
  viewer: "Can only watch.",
};

/** A "Share" button listing the keys this tab's key can hand out, as links. Whoever has a link has that role. */
function setupShare(role: Role, keys: Partial<Record<Role, string>>) {
  const button = document.getElementById("share-button")!;
  const panel = document.getElementById("share-panel")!;
  button.textContent = `${role} · share`;
  const rows = (["owner", "editor", "viewer"] as Role[])
    .filter((r) => keys[r])
    .map((r) => {
      const row = document.createElement("div");
      const title = document.createElement("b");
      title.textContent = r;
      const blurb = document.createElement("span");
      blurb.textContent = ` ${ROLE_BLURB[r]}`;
      const copy = document.createElement("button");
      copy.textContent = "Copy link";
      copy.onclick = () => {
        void navigator.clipboard?.writeText(shareLink(keys[r]!));
        copy.textContent = "Copied";
      };
      row.append(title, blurb, copy);
      return row;
    });
  const note = document.createElement("p");
  note.textContent = "A link is a key: anyone who has it gets that role. Keys cannot be revoked.";
  // A new graph has a new owner key. Owner keys cannot be re-issued for an existing graph:
  // the editor and viewer keys are derived from it.
  const fresh = document.createElement("button");
  fresh.id = "new-graph-button";
  fresh.textContent = "New graph (new owner key)";
  fresh.onclick = async () => {
    fresh.disabled = true;
    try {
      const title = askTitle();
      if (title === null) return;
      const made = await createGraph(title);
      window.alert(
        `New graph "${title}". Keep these keys; they cannot be recovered.\n\nowner (everything, and can share the others):\n${made.owner}\n\neditor:\n${made.editor}\n\nviewer:\n${made.viewer}`,
      );
      window.open(shareLink(made.owner), "_blank");
    } catch (e) {
      window.alert(String(e));
    } finally {
      fresh.disabled = false;
    }
  };
  panel.append(...rows, note, fresh);
  button.onclick = () => (panel.style.display = panel.style.display === "block" ? "none" : "block");
}

/** The graph's name at the top left; the owner can click it to rename. The (title, key) pair is kept in this browser. */
function setupTitle(access: Access, graph: Graph) {
  const label = document.getElementById("doc-title")!;
  const show = () => {
    const title = graph.graphTitle();
    label.textContent = title || (access.role === "owner" ? "Untitled graph (click to name it)" : "Untitled graph");
    label.classList.toggle("unset", !title);
    document.title = title ? `${title} · Waypoint` : "Waypoint";
    rememberGraph(access, title);
  };
  graph.meta.observe(show);
  show();
  if (access.role !== "owner") return;
  label.classList.add("editable");
  label.title = "Rename this graph";
  label.onclick = async () => {
    const title = askTitle(graph.graphTitle());
    if (title === null) return;
    try {
      await setGraphTitle(access.key, title);
    } catch (e) {
      window.alert(String(e));
    }
  };
}

async function main() {
  await init();
  document.getElementById("for-agents")?.remove(); // the app is up; the text is only for readers without scripts

  // The key decides which graph opens and what we may do in it (a viewer only watches).
  const access = await getAccess();
  const canEdit = access.role !== "viewer";
  const graph = new Graph();
  graph.readOnly = !canEdit;
  const wsBase = `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`;
  const provider = new WebsocketProvider(wsBase, access.graph, graph.doc, { params: { key: access.key } });
  setupShare(access.role, access.keys);
  setupTitle(access, graph);
  const awareness = provider.awareness;

  const dpr = window.devicePixelRatio || 1;
  const app = new Application();
  await app.init({
    resizeTo: window,
    background: 0x14161a,
    antialias: true,
    // Render at the device's pixel density, otherwise text and lines are
    // upscaled and blurry on hidpi screens.
    resolution: dpr,
    autoDensity: true,
  });
  document.getElementById("canvas")!.appendChild(app.canvas);
  // Draw only when something changed (`applyCamera` does it), not 60 times a second: an idle page uses no CPU.
  app.ticker.stop();
  app.renderer.on("resize", () => app.render());

  const world = new Container();
  app.stage.addChild(world);
  const camera = new Camera();
  camera.x = -80;
  camera.y = -80;

  let labels: Text[] = [];
  let lastPositions: Record<string, Vec2> = {};
  let edgeSegments: { key: string; a: Vec2; b: Vec2 }[] = [];
  let localDrag: (Drag & { grabX: number; grabY: number }) | null = null;
  /** A drag that was dropped but whose move has not come back from the server yet; kept on screen so the node does not jump back and forth. */
  let dropped: Drag | null = null;
  let connecting: Connecting | null = null;
  let shiftDown = false;
  let panning = false;
  // View state, per user: not shared.
  let selectedEdge: string | null = null;
  let hoverNode: string | null = null;
  let selectedNode: string | null = null;
  /** The node whose title is being edited in the text box over it. */
  let editing: { id: string; original: string } | null = null;
  const editor = document.getElementById("editor") as HTMLTextAreaElement;

  const json = JSON.stringify;

  /** Stored places with every in-progress drag (collaborators', then ours) applied on top. */
  function effectivePlaces(): Record<string, Place> {
    const places = graph.places();
    const drags: Drag[] = [];
    awareness.getStates().forEach((state, clientId) => {
      const d = state.drag as Drag | null | undefined;
      if (clientId !== awareness.clientID && d && places[d.node]) drags.push(d);
    });
    if (dropped) drags.push(dropped);
    if (localDrag) drags.push(localDrag);
    for (const d of drags) {
      const changes: [string, Place][] = JSON.parse(
        move_node(json(places), d.node, Math.round(d.x), Math.round(d.y), d.subtree),
      );
      for (const [id, place] of changes) places[id] = place;
    }
    return places;
  }

  /** Text is rasterized once; re-rasterize when zoom outgrows it so it stays sharp. */
  function labelResolution(): number {
    const bucket = Math.min(4, Math.max(1, 2 ** Math.ceil(Math.log2(camera.zoom))));
    return dpr * bucket;
  }

  function nodeAt(w: Vec2, slack = 0): string | undefined {
    return Object.entries(lastPositions).find(
      ([, p]) => w.x >= p.x - slack && w.x <= p.x + NODE_W + slack && w.y >= p.y - slack && w.y <= p.y + NODE_H + slack,
    )?.[0];
  }

  /** `#12 title`, for messages. */
  function nameOf(id: string): string {
    const k = graph.key(id);
    return `${k === undefined ? "" : `#${k} `}${graph.title(id) || "untitled"}`;
  }

  /** What linking the connection's node to `target` would mean, and whether it is allowed. */
  function linkFor(c: Connecting, target: string): { from: string; to: string; problem?: string } {
    const [from, to] = [c.node, target];
    if (from === to) return { from, to, problem: "a node cannot depend on itself" };
    if (graph.edgePairs().some(([f, t]) => f === from && t === to)) return { from, to, problem: "already linked" };
    if (creates_cycle(json(graph.activePairs()), from, to)) return { from, to, problem: "that would create a dependency cycle" };
    return { from, to };
  }

  function render() {
    world.removeChildren().forEach((c) => c.destroy({ children: true }));
    labels = [];
    const positions: Record<string, Vec2> = JSON.parse(derive_positions(json(effectivePlaces())));
    // Nodes that move because of our own drag: whatever differs from the stored layout.
    const moving = new Set<string>();
    if (localDrag) {
      const stored: Record<string, Vec2> = JSON.parse(derive_positions(json(graph.places())));
      for (const [id, p] of Object.entries(positions)) {
        if (stored[id]?.x !== p.x || stored[id]?.y !== p.y) moving.add(id);
      }
    }
    lastPositions = positions;

    const centers = new Map<string, Vec2>();
    for (const [id, p] of Object.entries(positions)) centers.set(id, { x: p.x + NODE_W / 2, y: p.y + NODE_H / 2 });

    // Edges first, so nodes cover the line ends.
    const edges = new Graphics();
    edgeSegments = [];
    for (const e of graph.edges()) {
      const from = centers.get(e.from);
      const to = centers.get(e.to);
      if (!from || !to) continue;
      const a = boxExit(from, to);
      const b = boxExit(to, from);
      edgeSegments.push({ key: e.key, a, b });
      const draw = e.conflicted ? dashedArrow : arrow;
      if (e.key === selectedEdge) draw(edges, a, b, EDGE_SELECTED, 3.5);
      else draw(edges, a, b, e.conflicted ? EDGE_CONFLICTED : EDGE_COLOR);
    }
    if (selectedEdge && !edgeSegments.some((s) => s.key === selectedEdge)) selectedEdge = null; // removed elsewhere
    world.addChild(edges);

    // The link being drawn, and whether the node under the pointer would accept it.
    let linkTarget: { id: string; problem?: string } | null = null;
    const rubber = new Graphics();
    if (connecting) {
      const c = connecting;
      const p = positions[c.node];
      const start = { x: c.side === "left" ? p.x : p.x + NODE_W, y: p.y + NODE_H / 2 };
      const pointer = { x: c.x, y: c.y };
      const target = nodeAt(pointer);
      if (target && target !== c.node) linkTarget = { id: target, problem: linkFor(c, target).problem };
      const color = linkTarget ? (linkTarget.problem ? BAD : OK) : ACCENT;
      arrow(rubber, start, pointer, color);
    }

    const handleR = HANDLE_R / camera.zoom;
    const showHandles = connecting ? connecting.node : localDrag ? null : hoverNode;

    for (const [id, p] of Object.entries(positions)) {
      const node = new Container();
      node.position.set(p.x, p.y);

      const outline = linkTarget?.id === id
        ? { width: 3, color: linkTarget.problem ? BAD : OK }
        : moving.has(id)
          ? { width: 2, color: ACCENT }
          : id === selectedNode
            ? { width: 2, color: SELECTED }
            : { width: 1, color: 0x4a5568 };
      node.addChild(new Graphics().roundRect(0, 0, NODE_W, NODE_H, 8).fill(0x2b3340).stroke(outline));

      const title = graph.title(id);
      const label = new Text({
        text: title || "untitled",
        style: {
          fill: 0xe6e9ef,
          fontSize: 14,
          lineHeight: 17,
          fontFamily: "system-ui, sans-serif",
          wordWrap: true,
          wordWrapWidth: NODE_W - 24,
          breakWords: true,
        },
        resolution: labelResolution(),
      });
      label.alpha = title ? 1 : 0.4;
      const key = graph.key(id);
      label.position.set(12, NODE_H / 2 - Math.min(label.height, 34) / 2);
      // Long titles are clipped to two lines; the full text is in the editor.
      const clip = new Graphics().rect(8, 5, NODE_W - 16, NODE_H - 10).fill(0xffffff);
      label.mask = clip;
      node.addChild(clip, label);
      labels.push(label);
      if (key !== undefined) {
        // The short key (#12): how people and agents refer to this node. It never changes, even on rename.
        const badge = new Text({
          text: `#${key}`,
          style: { fill: 0x8793a8, fontSize: 10, fontFamily: "system-ui, sans-serif" },
          resolution: labelResolution(),
        });
        badge.anchor.set(1, 1);
        badge.position.set(NODE_W - 8, NODE_H - 4);
        node.addChild(badge);
        labels.push(badge);
      }
      if (graph.note(id).trim()) {
        // The note lives in the margin; this shows that there is one.
        const count = new Text({
          text: "✎",
          style: { fill: 0x8793a8, fontSize: 10, fontFamily: "system-ui, sans-serif" },
          resolution: labelResolution(),
        });
        count.anchor.set(0, 1);
        count.position.set(12, NODE_H - 4);
        node.addChild(count);
        labels.push(count);
      }

      if (showHandles === id) {
        const dots = new Graphics();
        for (const x of [0, NODE_W]) dots.circle(x, NODE_H / 2, handleR).fill(0x14161a).stroke({ width: 2 / camera.zoom, color: ACCENT });
        node.addChild(dots);
      }
      world.addChild(node);
    }
    world.addChild(rubber);

    updateHint(moving.size, linkTarget);
    applyCamera();
    margin?.update(selectedNode);
  }

  let margin: ReturnType<typeof setupMargin> | undefined;
  const hint = document.getElementById("hint")!;
  function updateHint(movingCount: number, linkTarget: { id: string; problem?: string } | null) {
    hint.classList.toggle("active", localDrag !== null || connecting !== null);
    if (connecting) {
      if (!linkTarget) {
        hint.innerHTML = "Drop on a node to link it, or on empty canvas to create a node it depends on";
      } else if (linkTarget.problem) {
        hint.innerHTML = `Can't link: ${linkTarget.problem}`;
      } else {
        const { from, to } = linkFor(connecting, linkTarget.id);
        hint.innerHTML = `Release to link: <b>${nameOf(from)}</b> → <b>${nameOf(to)}</b> (${nameOf(from)} depends on ${nameOf(to)})`;
      }
    } else if (localDrag) {
      if (localDrag.subtree) {
        hint.innerHTML =
          movingCount > 1
            ? `Moving <b>${movingCount} nodes</b> · hold <b>Shift</b> to move only this node`
            : "Moving <b>1 node</b> (nothing depends on it)";
      } else {
        hint.innerHTML = "Moving <b>only this node</b> · release <b>Shift</b> to take its sub-graph along";
      }
    } else if (!canEdit) {
      hint.innerHTML = "<b>Read-only</b> (viewer key) · drag the canvas to pan, scroll to zoom";
    } else if (selectedEdge && graph.edges().some((e) => e.key === selectedEdge && e.conflicted)) {
      hint.innerHTML = "This link would close a dependency cycle, so it is <b>ignored</b> · press <b>Delete</b> to remove it";
    } else if (selectedEdge) {
      hint.innerHTML = "Press <b>Delete</b> to remove the selected link";
    } else if (selectedNode) {
      hint.innerHTML = "<b>Enter</b> renames · <b>Ctrl+Enter</b> adds a dependant · <b>Delete</b> removes it (its links are joined) · <b>Esc</b> deselects";
    } else {
      hint.innerHTML =
        "<b>Double-click</b> empty canvas to add a node, a node to rename it · <b>Drag</b> a node to move it with its sub-graph (<b>Shift</b>: just the node)" +
        "<br>Drag from a node's <b>side dot</b> to another node to link them, or onto empty canvas to create one (the arrow points where you drop) · click a link, then <b>Delete</b>";
    }
  }

  function applyCamera() {
    world.position.set(-camera.x * camera.zoom, -camera.y * camera.zoom);
    world.scale.set(camera.zoom);
    const res = labelResolution();
    for (const l of labels) if (l.resolution !== res) l.resolution = res;
    positionEditor();
    app.render();
  }

  /** Keep the text box over its node. The camera is the one source of truth for where that is. */
  function positionEditor() {
    if (!editing) return;
    const at = lastPositions[editing.id];
    if (!at) return cancelEdit(); // the node was removed under us
    const s = camera.toScreen(at);
    const z = camera.zoom;
    Object.assign(editor.style, {
      left: `${s.x}px`,
      top: `${s.y}px`,
      width: `${NODE_W * z}px`,
      height: `${NODE_H * z}px`,
      fontSize: `${14 * z}px`,
      padding: `${(NODE_H / 2 - 17) * z}px ${12 * z}px`,
      borderRadius: `${8 * z}px`,
    });
  }

  function startEdit(id: string) {
    if (!lastPositions[id]) return;
    editing = { id, original: graph.title(id) };
    selectedNode = id;
    editor.value = editing.original;
    editor.style.display = "block";
    positionEditor();
    editor.focus();
    editor.select();
    render();
  }

  function endEdit() {
    editing = null;
    editor.style.display = "none";
    editor.blur();
  }

  /** This tab's name in the change log. Not an identity: anyone can send any name. */
  const actor = (() => {
    try {
      let a = sessionStorage.getItem("waypoint.actor");
      if (!a) sessionStorage.setItem("waypoint.actor", (a = `user-${Math.random().toString(36).slice(2, 6)}`));
      return a;
    } catch {
      return "user";
    }
  })();
  const headers = { Authorization: `Bearer ${access.key}`, "X-Waypoint-Actor": actor, "Content-Type": "application/json" };

  /**
   * Make a change through the same operations agents use. The server applies it
   * and the result comes back over the sync connection, which re-renders; this
   * returns the per-op results, or null if the server refused.
   */
  async function act(ops: object[], label?: string): Promise<{ id?: string }[] | null> {
    try {
      const r = await fetch(`/api/graphs/${access.graph}/apply`, {
        method: "POST",
        headers,
        body: JSON.stringify({ ops, label }),
      });
      if (!r.ok) {
        console.warn("waypoint: change refused:", (await r.json().catch(() => ({}))).error ?? r.status);
        render();
        return null;
      }
      return (await r.json()).results;
    } catch (e) {
      console.warn("waypoint: change failed:", e);
      return null;
    }
  }

  /** Undo or redo this tab's latest change. */
  async function stepHistory(redo: boolean) {
    await fetch(`/api/graphs/${access.graph}/${redo ? "redo" : "undo"}`, { method: "POST", headers }).catch(() => {});
  }

  /** Wait for a node made on the server to arrive over sync. */
  async function whenShown(id: string) {
    for (let i = 0; i < 100 && !lastPositions[id]; i++) await new Promise((r) => setTimeout(r, 20));
    return !!lastPositions[id];
  }

  function commitEdit() {
    if (!editing) return;
    const { id, original } = editing;
    endEdit();
    const title = editor.value.replace(/\s+/g, " ").trim().slice(0, 200);
    if (title && title !== original) void act([{ op: "rename_node", node: id, title }], "rename");
    else render();
  }

  function cancelEdit() {
    if (!editing) return;
    endEdit();
    render();
  }

  editor.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
      e.preventDefault();
      const id = editing?.id;
      commitEdit();
      if (id) createDependant(id);
    } else if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      commitEdit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      cancelEdit();
    }
    e.stopPropagation(); // the canvas shortcuts (Delete, Enter, ...) must not see typing
  });
  editor.addEventListener("blur", commitEdit);

  type PointerLike = { button: number; clientX: number; clientY: number; shiftKey: boolean };
  const worldOf = (e: { clientX: number; clientY: number }) => camera.toWorld({ x: e.clientX, y: e.clientY });

  /**
   * A press on the canvas, resolved by our own hit-testing rather than
   * PixiJS's per-object events: those need the pointer to have moved onto
   * the object first, which a tap or a fast click does not do.
   */
  function onCanvasPress(e: PointerLike) {
    if (e.button !== 0) return;
    const w = worldOf(e);
    const reach = HANDLE_HIT / camera.zoom;
    const id = nodeAt(w, reach);
    const at = id ? lastPositions[id] : undefined;
    if (id && at && canEdit) {
      // Grabbing a side dot starts a link; anywhere else on the node starts a move.
      const mid = at.y + NODE_H / 2;
      if (Math.hypot(w.x - at.x, w.y - mid) <= reach) return startConnect(id, "left", w);
      if (Math.hypot(w.x - (at.x + NODE_W), w.y - mid) <= reach) return startConnect(id, "right", w);
      if (nodeAt(w) === id) {
        selectedNode = id;
        return startDrag(id, at, w, e);
      }
    }
    if (id && at && !canEdit) {
      // A viewer cannot move or link, but can select a node to read its note.
      if (selectedNode !== id || selectedEdge) {
        selectedNode = id;
        selectedEdge = null;
        render();
      }
      return;
    }
    // Empty canvas: select the link under the pointer, if any, and start panning.
    const edgeReach = EDGE_HIT / camera.zoom;
    let best: { key: string; d: number } | null = null;
    for (const s of edgeSegments) {
      const d = distanceToSegment(w, s.a, s.b);
      if (d <= edgeReach && (!best || d < best.d)) best = { key: s.key, d };
    }
    const picked = best?.key ?? null;
    if (picked !== selectedEdge || selectedNode) {
      selectedEdge = picked;
      selectedNode = null;
      render();
    }
    panning = true;
  }

  /** Distance between two world points, in screen pixels. */
  function cameraDistance(a: Vec2, b: Vec2): number {
    return Math.hypot(a.x - b.x, a.y - b.y) * camera.zoom;
  }

  function startConnect(id: string, side: "left" | "right", w: Vec2) {
    connecting = { node: id, side, x: w.x, y: w.y, fromX: w.x, fromY: w.y };
    selectedEdge = null;
    render();
  }

  function startDrag(id: string, at: Vec2, w: Vec2, e: PointerLike) {
    localDrag = { node: id, x: at.x, y: at.y, subtree: !e.shiftKey, grabX: w.x - at.x, grabY: w.y - at.y };
    shiftDown = e.shiftKey;
    selectedEdge = null;
    publishDrag();
    render();
  }

  function publishDrag() {
    const d = localDrag;
    awareness.setLocalStateField("drag", d && { node: d.node, x: d.x, y: d.y, subtree: d.subtree });
  }

  /** Delete the selected node. Chains through it are joined; nothing else moves. */
  function deleteSelectedNode() {
    if (!selectedNode) return;
    const id = selectedNode;
    selectedNode = null;
    void act([{ op: "delete_node", node: id }], "delete node");
  }

  function removeSelectedEdge() {
    if (!selectedEdge) return;
    const pair = graph.edgePairs(selectedEdge)[0];
    selectedEdge = null;
    if (pair) void act([{ op: "unlink", from: pair[0], to: pair[1] }], "remove link");
    else render();
  }

  // Dragging a node moves it with its sub-graph; hold Shift to move just the node.
  // Dragging from a side dot links; dragging the background pans, the wheel zooms.
  app.canvas.addEventListener("pointerdown", onCanvasPress);

  window.addEventListener("pointerup", (e) => {
    panning = false;
    if (connecting) {
      const c = connecting;
      connecting = null;
      const w = worldOf(e);
      const target = nodeAt(w);
      if (target && target !== c.node) {
        const link = linkFor(c, target);
        if (!link.problem) void act([{ op: "link", from: link.from, to: link.to }], "link");
        else render();
      } else if (!target && cameraDistance({ x: c.fromX, y: c.fromY }, w) > 24) {
        createNode(w, c.node); // dropped on empty canvas: a new node the start node depends on
      } else {
        render(); // released on its own node, or barely moved: nothing to do
      }
      return;
    }
    if (!localDrag) return;
    const d = localDrag;
    localDrag = null;
    publishDrag();
    const shown: Record<string, Vec2> = JSON.parse(derive_positions(json(effectivePlaces()))); // as dropped
    dropped = { node: d.node, x: d.x, y: d.y, subtree: d.subtree };
    void act(
      [{ op: "move_node", node: d.node, x: Math.round(d.x), y: Math.round(d.y), with_subtree: d.subtree }],
      "move",
    ).then(async (done) => {
      // Keep the preview until the stored layout shows the same place (or the move was refused).
      for (let i = 0; done && i < 40; i++) {
        const stored: Record<string, Vec2> = JSON.parse(derive_positions(json(graph.places())));
        if (stored[d.node]?.x === shown[d.node]?.x && stored[d.node]?.y === shown[d.node]?.y) break;
        await new Promise((r) => setTimeout(r, 50));
      }
      dropped = null;
      render();
    });
    render();
  });

  window.addEventListener("pointermove", (e) => {
    if (connecting) {
      const w = worldOf(e);
      connecting.x = w.x;
      connecting.y = w.y;
      render();
    } else if (localDrag) {
      const w = worldOf(e);
      localDrag.x = w.x - localDrag.grabX;
      localDrag.y = w.y - localDrag.grabY;
      localDrag.subtree = !e.shiftKey;
      shiftDown = e.shiftKey;
      publishDrag();
      render();
    } else if (panning) {
      camera.panBy(e.movementX, e.movementY);
      applyCamera();
    } else {
      // Show the side dots on the node under the pointer (with a little slack so
      // the dots, which stick out of the node, can be reached).
      const hover = nodeAt(worldOf(e), HANDLE_HIT / camera.zoom) ?? null;
      if (hover !== hoverNode) {
        hoverNode = hover;
        render();
      }
    }
  });

  /**
   * Add a node centred at `at`, with the layout making room for it, and open
   * its title for typing. `dependedOnBy` links it: that node depends on the new one.
   */
  async function createNode(at: Vec2, dependedOnBy?: string) {
    const op = { op: "create_node", title: NEW_TITLE, x: Math.round(at.x - NODE_W / 2), y: Math.round(at.y - NODE_H / 2), dependants: dependedOnBy ? [dependedOnBy] : [] };
    await createAndEdit(op);
  }

  /** Add a node that depends on `parent`, next to it in the flow direction, and open its title. */
  async function createDependant(parent: string) {
    await createAndEdit({ op: "create_node", title: NEW_TITLE, depends_on: [parent] });
  }

  async function createAndEdit(op: object) {
    const id = (await act([op], "add node"))?.[0]?.id;
    if (id && (await whenShown(id))) startEdit(id);
  }

  // Double-click a node to rename it, or empty canvas to add a node there.
  app.canvas.addEventListener("dblclick", (e) => {
    const w = worldOf(e);
    if (!canEdit) return;
    const hit = nodeAt(w);
    if (hit) startEdit(hit);
    else createNode(w);
  });

  const onKey = (e: KeyboardEvent) => {
    // Shift pressed or released mid-drag, without moving the mouse.
    if (localDrag && e.key === "Shift" && e.shiftKey !== shiftDown) {
      shiftDown = e.shiftKey;
      localDrag.subtree = !e.shiftKey;
      publishDrag();
      render();
    }
    if (e.target instanceof Element && e.target.closest("#margin")) return; // typing in a note
    if (e.type !== "keydown" || editing || !canEdit) return;
    if ((e.key === "Delete" || e.key === "Backspace") && selectedEdge) {
      e.preventDefault();
      removeSelectedEdge();
    } else if ((e.key === "Delete" || e.key === "Backspace") && selectedNode) {
      e.preventDefault();
      deleteSelectedNode();
    } else if ((e.key === "F2" || (e.key === "Enter" && !e.ctrlKey && !e.metaKey)) && selectedNode) {
      e.preventDefault();
      startEdit(selectedNode);
    } else if (e.key === "Enter" && (e.ctrlKey || e.metaKey) && selectedNode && lastPositions[selectedNode]) {
      e.preventDefault();
      createDependant(selectedNode);
    } else if ((e.ctrlKey || e.metaKey) && (e.key === "z" || e.key === "Z" || e.key === "y")) {
      e.preventDefault();
      void stepHistory(e.key === "y" || e.shiftKey);
    } else if (e.key === "Escape" && (selectedEdge || selectedNode || connecting)) {
      selectedEdge = null;
      selectedNode = null;
      connecting = null;
      render();
    }
  };
  window.addEventListener("keydown", onKey);
  window.addEventListener("keyup", onKey);

  app.canvas.addEventListener(
    "wheel",
    (e) => {
      e.preventDefault();
      camera.zoomAt({ x: e.offsetX, y: e.offsetY }, e.deltaY < 0 ? 1.1 : 1 / 1.1);
      render(); // handle sizes depend on zoom
    },
    { passive: false },
  );

  margin = setupMargin(document.getElementById("margin")!, {
    graph,
    canEdit,
    select: (id) => {
      selectedNode = id;
      selectedEdge = null;
      render();
    },
  });
  graph.doc.on("update", render);
  awareness.on("change", render);
  render();

  if (import.meta.env.DEV) {
    // Handle for automated browser tests: where nodes are on screen, and the document.
    (window as unknown as Record<string, unknown>).__waypoint = {
      graph,
      nodeIds: () => Object.keys(lastPositions),
      state: () => ({ selectedNode, selectedEdge, editing: editing?.id ?? null, dragging: localDrag?.node ?? null }),
      screenRect: (id: string) => {
        const at = lastPositions[id];
        const s = camera.toScreen(at);
        return { x: s.x, y: s.y, w: NODE_W * camera.zoom, h: NODE_H * camera.zoom };
      },
    };
  }
}

main();
