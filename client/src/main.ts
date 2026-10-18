import { Application, CanvasTextMetrics, Container, Graphics, Text, TextStyle } from "pixi.js";
import { WebsocketProvider } from "y-websocket";
import init, {
  creates_cycle,
  derive_positions,
  move_group,
  move_node,
} from "./wasm/waypoint_layout.js";
import { createGraph, getAccess, linkFor as shareLink, rememberGraph, setGraphTitle, type Access, type Role } from "./access";
import { Camera } from "./camera";
import { drawEdge, exitPoint, isSharp, outline as shapeOutline, strokePath } from "./draw";
import { exportPng, exportSvg, fileName } from "./export";
import { NODE_H, NODE_W, type Scene } from "./scene";
import { LINE_HEIGHT, layoutTitle } from "./wrap";
import { Graph, type Place, type Vec2 } from "./graph";
import { setupMargin } from "./margin";
import { edgeSwatch, setupTypes } from "./types";
import { hexNum, readableOn } from "./style";
import { applyTheme } from "./theme";

const NEW_TITLE = "New node";

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

/**
 * Angle to draw text along the segment `a`→`b`: the segment's own angle, turned
 * half a revolution when needed so the text's up never points downwards
 * (dot(text_up, up) >= 0, i.e. cos(angle) >= 0 with y pointing down).
 */
function upright(a: Vec2, b: Vec2): number {
  const angle = Math.atan2(b.y - a.y, b.x - a.x);
  return Math.cos(angle) < 0 ? angle + Math.PI : angle;
}

const FONT = "system-ui, sans-serif";
const EMOJI_FONT = `${FONT}, "Apple Color Emoji", "Segoe UI Emoji", "Noto Color Emoji"`;
/** How long the hint at the bottom stays when nothing is happening. */
const HINT_IDLE_MS = 5000;

const EDGE_LABEL_GAP = 8; // from the line to the text's centre, along the text's up

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
  /** Take what the node depends on along (Shift-drag); a plain drag moves only the node. */
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
      const made = await createGraph();
      // Go to the new graph in this tab. Only the hash differs, so the page must be reloaded to read it.
      location.assign(shareLink(made.owner));
      location.reload();
    } catch (e) {
      note.textContent = String(e);
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
  const owner = access.role === "owner";
  const show = () => {
    const title = graph.graphTitle();
    label.textContent = title || "Unnamed";
    if (!title && owner) {
      const hint = document.createElement("span");
      hint.textContent = " · click to name this graph";
      hint.style.fontWeight = "400";
      label.append(hint);
    }
    label.classList.toggle("unset", !title);
    document.title = title ? `${title} · Waypoint` : "Waypoint";
    rememberGraph(access, title);
  };
  graph.meta.observe(show);
  show();
  if (!owner) return;
  label.classList.add("editable");
  label.title = "Rename this graph";
  label.onclick = () => {
    if (label.querySelector("input")) return;
    const input = document.createElement("input");
    input.value = graph.graphTitle();
    input.placeholder = "Name this graph";
    input.maxLength = 200;
    input.style.cssText = "font:inherit;color:var(--fg);background:var(--panel);border:1px solid var(--accent);border-radius:4px;padding:0 4px;width:min(320px,38vw);outline:none";
    let finished = false;
    const finish = async (save: boolean) => {
      if (finished) return;
      finished = true;
      const title = input.value.trim();
      input.remove();
      show();
      if (!save || !title || title === graph.graphTitle()) return;
      try {
        await setGraphTitle(access.key, title);
      } catch (e) {
        label.textContent = String(e);
        setTimeout(show, 3000);
      }
    };
    input.onkeydown = (ev) => {
      ev.stopPropagation();
      if (ev.key === "Enter") void finish(true);
      else if (ev.key === "Escape") void finish(false);
    };
    input.onblur = () => void finish(true);
    label.textContent = "";
    label.append(input);
    input.focus();
    input.select();
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
  let lastBackground = "";
  /** The side dots of the node under the pointer. Separate from `render`, so hovering and zooming do not rebuild every label. */
  let handles = new Graphics();
  let edgeSegments: { key: string; a: Vec2; b: Vec2 }[] = [];
  let localDrag: (Drag & { grabX: number; grabY: number }) | null = null;
  /** A drag that was dropped but whose move has not come back from the server yet; kept on screen so the node does not jump back and forth. */
  let dropped: Drag | null = null;
  let connecting: Connecting | null = null;
  /** A link the user has drawn and is choosing the type of (the picker is open). */
  let pendingLink: { from: string; to: string } | null = null;
  let picker: HTMLElement | null = null;
  /** What is on the canvas, as of the last draw; the export reads it. */
  let lastScene: Scene = { background: "", nodes: [], edges: [] };
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
        d.subtree
          ? move_group(json(places), json(graph.edgePairs()), d.node, Math.round(d.x), Math.round(d.y))
          : move_node(json(places), d.node, Math.round(d.x), Math.round(d.y), false),
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
    return `${k === undefined ? "" : `#${k} `}${graph.title(id).replace(/\s*\n\s*/g, " ") || "untitled"}`;
  }

  /** What linking the connection's node to `target` would mean, and whether it is allowed. */
  function linkFor(c: Connecting, target: string): { from: string; to: string; problem?: string } {
    const [from, to] = [c.node, target];
    if (from === to) return { from, to, problem: "a node cannot depend on itself" };
    if (graph.edgePairs().some(([f, t]) => f === from && t === to)) return { from, to, problem: "already linked" };
    if (creates_cycle(json(graph.activePairs()), from, to)) return { from, to, problem: "that would create a dependency cycle" };
    return { from, to };
  }

  /** Text widths, measured once: breaking a title into lines measures many candidate lines. */
  const measureStyle = new TextStyle({ fontSize: 14, fontFamily: FONT });
  const widths = new Map<string, number>();
  const measure = (text: string): number => {
    let w = widths.get(text);
    if (w === undefined) {
      if (widths.size > 20000) widths.clear();
      w = CanvasTextMetrics.measureText(text, measureStyle).width;
      widths.set(text, w);
    }
    return w;
  };
  const brokenTitles = new Map<string, string[]>();
  /** A title broken into lines that fit a node of this shape (manual line breaks kept); the full title is in the editor. */
  function titleLines(title: string, shape: string): string[] {
    const k = `${shape}\0${title}`;
    let lines = brokenTitles.get(k);
    if (!lines) {
      if (brokenTitles.size > 2000) brokenTitles.clear();
      lines = layoutTitle(title, shape, NODE_W, NODE_H, measure);
      brokenTitles.set(k, lines);
    }
    return lines;
  }

  function drawHandles() {
    handles.clear();
    const showHandles = connecting ? connecting.node : localDrag ? null : hoverNode;
    const p = showHandles ? lastPositions[showHandles] : undefined;
    if (!p) return;
    const handleR = HANDLE_R / camera.zoom;
    for (const x of [0, NODE_W]) handles.circle(p.x + x, p.y + NODE_H / 2, handleR).fill(hexNum(lastBackground)).stroke({ width: 2 / camera.zoom, color: ACCENT });
  }

  function render() {
    world.removeChildren().forEach((c) => c.destroy({ children: true }));
    labels = [];
    const reg = graph.registry();
    app.renderer.background.color = hexNum(reg.background);
    document.documentElement.style.background = reg.background;
    applyTheme(reg.background);
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
    lastBackground = reg.background;

    const centers = new Map<string, Vec2>();
    for (const [id, p] of Object.entries(positions)) centers.set(id, { x: p.x + NODE_W / 2, y: p.y + NODE_H / 2 });
    // Where a link meets a node is on the node's real outline (an ellipse or rhombus does not reach its box corners).
    const outlines = new Map<string, Vec2[]>();
    for (const [id, p] of Object.entries(positions)) {
      const t = reg.nodeType(id);
      outlines.set(id, shapeOutline(t.shape, t.corners, NODE_W, NODE_H).map((q) => ({ x: q.x + p.x, y: q.y + p.y })));
    }
    const exitOf = (id: string, toward: Vec2): Vec2 => exitPoint(outlines.get(id)!, centers.get(id)!, toward);
    const scene: Scene = { background: reg.background, nodes: [], edges: [] };

    // Edges first, so nodes cover the line ends.
    const edges = new Graphics();
    edgeSegments = [];
    const edgeLabels: Text[] = [];
    for (const e of graph.edges()) {
      const from = centers.get(e.from);
      const to = centers.get(e.to);
      if (!from || !to) continue;
      const a = exitOf(e.from, to);
      const b = exitOf(e.to, from);
      edgeSegments.push({ key: e.key, a, b });
      const type = reg.edgeType(e.type);
      if (e.key === selectedEdge) drawEdge(edges, a, b, type, { color: EDGE_SELECTED, width: Math.max(3.5, type.width + 1.5) });
      else if (e.conflicted) drawEdge(edges, a, b, type, { color: EDGE_CONFLICTED, style: "dashed" });
      else drawEdge(edges, a, b, type);

      const sceneEdge = { a, b, type, conflicted: e.conflicted, label: null as { x: number; y: number; angle: number } | null };
      scene.edges.push(sceneEdge);
      if (!type.label) continue; // an empty label draws nothing
      const label = new Text({
        text: type.label,
        style: { fill: e.conflicted ? EDGE_CONFLICTED : hexNum(type.text_color), fontSize: 11, fontFamily: FONT },
        resolution: labelResolution(),
      });
      if (Math.hypot(b.x - a.x, b.y - a.y) < label.width + 2 * ARROW_LEN + 8) {
        label.destroy(); // no room along this link
        continue;
      }
      const angle = upright(a, b);
      label.anchor.set(0.5);
      label.rotation = angle;
      // Sit on the text's upper side, so the line does not strike through it.
      label.position.set(
        (a.x + b.x) / 2 + Math.sin(angle) * EDGE_LABEL_GAP,
        (a.y + b.y) / 2 - Math.cos(angle) * EDGE_LABEL_GAP,
      );
      sceneEdge.label = { x: label.position.x, y: label.position.y, angle };
      edgeLabels.push(label);
      labels.push(label);
    }
    if (selectedEdge && !edgeSegments.some((s) => s.key === selectedEdge)) selectedEdge = null; // removed elsewhere
    world.addChild(edges, ...edgeLabels);

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
    } else if (pendingLink) {
      // The link is drawn and waiting for its type to be picked.
      const [f, t] = [centers.get(pendingLink.from), centers.get(pendingLink.to)];
      if (f && t) arrow(rubber, exitOf(pendingLink.from, t), exitOf(pendingLink.to, f), OK);
    }

    for (const [id, p] of Object.entries(positions)) {
      const node = new Container();
      node.position.set(p.x, p.y);

      // The type's border, unless the node is being linked to, moved or selected.
      const type = reg.nodeType(id);
      const state = linkTarget?.id === id
        ? { width: 3, color: linkTarget.problem ? BAD : OK }
        : moving.has(id)
          ? { width: 2, color: ACCENT }
          : id === selectedNode
            ? { width: 2, color: SELECTED }
            : null;
      const shape = shapeOutline(type.shape, type.corners, NODE_W, NODE_H);
      const join = isSharp(type.shape, type.corners) ? "miter" : "round";
      const body = new Graphics().poly(shape).fill(hexNum(type.bg));
      if (state) strokePath(body, shape, true, state.color, state.width, "solid", 1, join);
      else strokePath(body, shape, true, hexNum(type.border), type.border_width, "solid", 1, join);
      node.addChild(body);

      // The title, centred, already broken into lines that fit this shape.
      const title = graph.title(id);
      const lines = title ? titleLines(title, type.shape) : ["untitled"];
      const titleStyle = new TextStyle({ fill: hexNum(type.fg), fontSize: 14, lineHeight: LINE_HEIGHT, fontFamily: FONT, align: "center" });
      const label = new Text({ text: lines.join("\n"), style: titleStyle, resolution: labelResolution() });
      label.alpha = title ? 1 : 0.4;
      label.anchor.set(0.5);
      label.position.set(NODE_W / 2, NODE_H / 2);
      node.addChild(label);
      labels.push(label);
      const key = graph.key(id);
      const hasNote = graph.note(id).trim() !== "";
      scene.nodes.push({ id, x: p.x, y: p.y, type, lines, untitled: !title, key, hasNote });
      if (key !== undefined) {
        // The short key (#12): how people and agents refer to this node. It never changes, even on rename.
        // It sits just below the node (a two-line title can fill the corner, and the corners of most shapes are empty canvas),
        // so it is coloured for the canvas, not for the node.
        const badge = new Text({
          text: `#${key}`,
          style: { fill: hexNum(readableOn(reg.background)), fontSize: 10, fontFamily: FONT },
          resolution: labelResolution(),
        });
        badge.alpha = 0.7;
        badge.anchor.set(1, 1);
        badge.position.set(NODE_W, NODE_H + 12);
        node.addChild(badge);
        labels.push(badge);
      }
      if (hasNote) {
        // The note lives in the margin; this shows that there is one. It hangs off the node's top-left corner, clear of the title.
        const icon = new Text({ text: "🗒", style: { fontSize: 12, fontFamily: EMOJI_FONT }, resolution: labelResolution() });
        icon.alpha = 0.85;
        icon.anchor.set(0, 1);
        icon.position.set(2, -3);
        node.addChild(icon);
        labels.push(icon);
      }

      world.addChild(node);
    }
    world.addChild(rubber);
    lastScene = scene;
    handles = new Graphics();
    world.addChild(handles);
    drawHandles();

    updateHint(moving.size, linkTarget);
    applyCamera();
    margin?.update(selectedNode);
    typesPanel?.update();
  }

  let typesPanel: ReturnType<typeof setupTypes> | undefined;

  let margin: ReturnType<typeof setupMargin> | undefined;
  const hint = document.getElementById("hint")!;
  // The hint at the bottom is for someone who is doing something: it fades after a few idle seconds and returns on the next action.
  hint.style.transition = "opacity 0.4s";
  let hintTimer = 0;
  function wake() {
    hint.style.opacity = "";
    window.clearTimeout(hintTimer);
    hintTimer = window.setTimeout(fadeHint, HINT_IDLE_MS);
  }
  function fadeHint() {
    if (localDrag || connecting || picker) return wake(); // holding still in the middle of an action is not idle
    hint.style.opacity = "0";
  }
  for (const type of ["pointermove", "pointerdown", "keydown", "wheel", "touchstart"]) window.addEventListener(type, wake, { passive: true, capture: true });
  wake();
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
            ? `Moving <b>${movingCount} nodes</b> (the node and what it depends on) · release <b>Shift</b> to move only this node`
            : "Moving <b>1 node</b> (it depends on nothing)";
      } else {
        hint.innerHTML = "Moving <b>only this node</b> · hold <b>Shift</b> to take what it depends on along";
      }
    } else if (editing) {
      hint.innerHTML = "<b>Enter</b> saves · <b>Shift+Enter</b> starts a new line · <b>Esc</b> cancels";
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
        "<b>Double-click</b> empty canvas to add a node, a node to rename it · <b>Drag</b> a node to move it (<b>Shift</b>-drag: with what it depends on)" +
        "<br>Drag from a node's <b>side dot</b> to another node to link them, or onto empty canvas to create one (the arrow points where you drop) · click a link, then <b>Delete</b>";
    }
  }

  /** `settle` false skips re-rasterizing the text for the new zoom (slow): done once zooming stops. */
  function applyCamera(settle = true) {
    world.position.set(-camera.x * camera.zoom, -camera.y * camera.zoom);
    world.scale.set(camera.zoom);
    if (settle) {
      const res = labelResolution();
      for (const l of labels) if (l.resolution !== res) l.resolution = res;
    }
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
      lineHeight: `${LINE_HEIGHT * z}px`,
      textAlign: "center",
      padding: `${(NODE_H / 2 - LINE_HEIGHT) * z}px ${12 * z}px`,
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

  /** This tab's name in the change log. Names are for people to read: they may repeat and prove nothing. */
  const actor = (() => {
    try {
      let a = sessionStorage.getItem("waypoint.actor");
      if (!a) sessionStorage.setItem("waypoint.actor", (a = `user-${Math.random().toString(36).slice(2, 6)}`));
      return a;
    } catch {
      return "user";
    }
  })();
  /** This tab's id: it keys the tab's undo history, so tabs with the same name are still different peers. */
  const peer = (() => {
    try {
      let p = sessionStorage.getItem("waypoint.peer");
      if (!p) sessionStorage.setItem("waypoint.peer", (p = crypto.randomUUID()));
      return p;
    } catch {
      return crypto.randomUUID();
    }
  })();
  const headers = { Authorization: `Bearer ${access.key}`, "X-Waypoint-Actor": actor, "X-Waypoint-Peer": peer, "Content-Type": "application/json" };

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
    // Line breaks the person typed (Shift+Enter) stay; spaces are tidied within each line, empty lines dropped.
    const title = editor.value
      .split("\n")
      .map((l) => l.replace(/\s+/g, " ").trim())
      .filter(Boolean)
      .join("\n")
      .slice(0, 200);
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
    localDrag = { node: id, x: at.x, y: at.y, subtree: e.shiftKey, grabX: w.x - at.x, grabY: w.y - at.y };
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
    // The key is "from>to". (`edgePairs(key)` would give every link except this one.)
    const [from, to] = selectedEdge.split(">");
    const exists = graph.edges().some((e) => e.from === from && e.to === to);
    selectedEdge = null;
    if (from && to && exists) void act([{ op: "unlink", from, to }], "remove link");
    else render();
  }

  // Dragging a node moves just that node; hold Shift to take what it depends on along.
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
        if (!link.problem) chooseEdgeType({ from: link.from, to: link.to }, { x: e.clientX, y: e.clientY });
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
      [{ op: "move_node", node: d.node, x: Math.round(d.x), y: Math.round(d.y), with_dependencies: d.subtree }],
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
      localDrag.subtree = e.shiftKey;
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
        drawHandles();
        applyCamera();
      }
    }
  });

  /** Close the edge type list without making the link. */
  function closePicker() {
    picker?.remove();
    picker = null;
    if (pendingLink) {
      pendingLink = null;
      render();
    }
  }

  /**
   * A new link needs a type: list the edge types at the pointer and make the link
   * with the one picked. (With only one type there is nothing to pick.)
   */
  function chooseEdgeType(link: { from: string; to: string }, at: { x: number; y: number }) {
    const reg = graph.registry();
    if (reg.edgeTypes.length <= 1) {
      void act([{ op: "link", from: link.from, to: link.to }], "link");
      return;
    }
    closePicker();
    pendingLink = link;
    const box = document.createElement("div");
    box.className = "edge-picker";
    box.setAttribute("role", "menu");
    box.style.cssText =
      "position:fixed;z-index:50;min-width:200px;max-width:320px;max-height:70vh;overflow:auto;padding:6px;border-radius:8px;" +
      "background:var(--panel);color:var(--fg);border:1px solid var(--line);box-shadow:0 6px 24px rgba(0,0,0,.35);font:13px system-ui,sans-serif";
    const head = document.createElement("div");
    head.textContent = `${nameOf(link.from)} depends on ${nameOf(link.to)}: which kind of link?`;
    head.style.cssText = "padding:2px 8px 6px;color:var(--muted);font-size:12px";
    box.append(head);
    const buttons: HTMLButtonElement[] = [];
    for (const t of reg.edgeTypes) {
      const b = document.createElement("button");
      b.type = "button";
      b.setAttribute("role", "menuitem");
      b.style.cssText = "display:flex;align-items:center;gap:8px;width:100%;padding:4px 8px;border:0;border-radius:6px;background:transparent;color:inherit;font:inherit;text-align:left;cursor:pointer";
      const name = document.createElement("span");
      name.textContent = t.name;
      b.append(edgeSwatch(t, reg.background), name);
      b.onmouseenter = () => b.focus();
      b.onclick = () => {
        const l = pendingLink;
        box.remove();
        picker = null;
        pendingLink = null;
        if (l) void act([{ op: "link", from: l.from, to: l.to, edge_type: t.name }], "link");
        render();
      };
      buttons.push(b);
      box.append(b);
    }
    box.addEventListener("keydown", (ev) => {
      ev.stopPropagation(); // the canvas shortcuts must not see these keys
      const i = buttons.indexOf(document.activeElement as HTMLButtonElement);
      if (ev.key === "Escape") {
        ev.preventDefault();
        closePicker();
      } else if (ev.key === "ArrowDown" || ev.key === "ArrowUp") {
        ev.preventDefault();
        buttons[(i + (ev.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length]!.focus();
      }
    });
    document.body.append(box);
    picker = box;
    const r = box.getBoundingClientRect();
    box.style.left = `${Math.max(8, Math.min(at.x + 8, window.innerWidth - r.width - 8))}px`;
    box.style.top = `${Math.max(8, Math.min(at.y + 8, window.innerHeight - r.height - 8))}px`;
    buttons[0]!.focus();
    render();
  }
  // A press anywhere outside the list closes it.
  window.addEventListener(
    "pointerdown",
    (e) => {
      if (picker && !(e.target instanceof Node && picker.contains(e.target))) closePicker();
    },
    true,
  );

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
      localDrag.subtree = e.shiftKey;
      publishDrag();
      render();
    }
    if (e.target instanceof Element && e.target.closest("#margin, #types-panel")) return; // typing in a note or the types panel
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

  // Zoom. Firefox sends a burst of wheel events per swipe; redrawing for each one backs the events up behind slow
  // frames, and the zoom then keeps running long after the swipe ended. So events only add up what they asked for
  // (by how far the wheel turned, not how many events came), and one frame applies the sum.
  let wheelZoom = 0; // log of the zoom factor asked for since the last frame
  let wheelAt: Vec2 = { x: 0, y: 0 };
  let wheelFrame = 0;
  let zoomSettle = 0;
  function applyWheel() {
    wheelFrame = 0;
    const ask = Math.max(-0.4, Math.min(0.4, wheelZoom));
    wheelZoom = 0;
    camera.zoomAt(wheelAt, Math.exp(ask));
    drawHandles(); // handle sizes depend on zoom
    applyCamera(false);
    // Sharpen the text once the zoom has stopped.
    window.clearTimeout(zoomSettle);
    zoomSettle = window.setTimeout(() => applyCamera(true), 150);
  }
  app.canvas.addEventListener(
    "wheel",
    (e) => {
      e.preventDefault();
      const pixels = e.deltaY * (e.deltaMode === 1 ? 16 : e.deltaMode === 2 ? 400 : 1);
      wheelZoom -= Math.max(-120, Math.min(120, pixels)) * 0.001;
      wheelAt = { x: e.offsetX, y: e.offsetY };
      if (!wheelFrame) wheelFrame = requestAnimationFrame(applyWheel);
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
  /** Send a request that changes the graph; resolves to the server's error text, or null if it went through. */
  async function change(path: string, method: string, body: object): Promise<string | null> {
    try {
      const r = await fetch(`/api/graphs/${access.graph}/${path}`, { method, headers, body: JSON.stringify(body) });
      if (r.ok) return null;
      return (await r.json().catch(() => ({}))).error ?? `refused (${r.status})`;
    } catch (e) {
      return `failed: ${String(e)}`;
    }
  }
  typesPanel = setupTypes(document.getElementById("types-panel")!, document.getElementById("types-button")!, {
    graph,
    canEdit,
    isOwner: access.role === "owner",
    send: (ops) => change("apply", "POST", { ops, label: "types" }),
    setBackground: (color) => change("background", "PUT", { color }),
    selectedNode: () => selectedNode,
    selectedEdge: () => {
      if (!selectedEdge) return null;
      const [from, to] = selectedEdge.split(">");
      return from && to ? { from, to } : null;
    },
  });
  // Export buttons next to "Types", styled like it. They save what is on the canvas, cropped to the graph.
  const typesButton = document.getElementById("types-button")!;
  const exportButton = (label: string, id: string, run: () => Promise<boolean> | boolean) => {
    const b = document.createElement("button");
    b.id = id;
    b.className = typesButton.className.replace(/\bon\b/, "").trim();
    b.textContent = label;
    b.onclick = async () => {
      try {
        if (!(await run())) hint.textContent = "Nothing to export yet: the graph is empty";
      } catch (err) {
        hint.textContent = `Export failed: ${String(err)}`;
      }
      wake();
    };
    return b;
  };
  typesButton.after(
    exportButton("Export SVG", "export-svg-button", () => exportSvg(lastScene, fileName(graph.graphTitle(), "svg"))),
    exportButton("Export PNG", "export-png-button", () => exportPng(lastScene, fileName(graph.graphTitle(), "png"))),
  );

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
