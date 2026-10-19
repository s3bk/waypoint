// The types panel: lists the node types and edge types, applies one to the
// selected node or link, and edits them. Every change is an op sent to the
// server (`upsert_node_type`, ...), which checks it; the form shows the
// contrast rules up front so Save is only offered for a style the server will take.

import { outline } from "./draw";
import type { Graph } from "./graph";
import {
  ARROWS,
  CORNERS,
  DEFAULT_EDGE_TYPE,
  DEFAULT_NODE_TYPE,
  type EdgeType,
  LINE_STYLES,
  MAX_WIDTH,
  MIN_CONTRAST,
  MIN_EDGE_WIDTH,
  MIN_LINE_CONTRAST,
  NODE_SHAPES,
  type NodeType,
  contrast,
  normalizeColor,
} from "./style";

export interface TypesHooks {
  graph: Graph;
  canEdit: boolean;
  isOwner: boolean;
  /** Send ops to the server; resolves to the error text, or null on success. */
  send: (ops: object[]) => Promise<string | null>;
  setBackground: (color: string) => Promise<string | null>;
  selectedNode: () => string | null;
  /** The selected link's ends, if one is selected. */
  selectedEdge: () => { from: string; to: string } | null;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string, text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function dashFor(style: string, width: number): string {
  return style === "dashed" ? `${width * 4} ${width * 2.5}` : style === "dotted" ? `0.1 ${width * 2.2}` : "";
}

function nodeSwatch(t: NodeType): HTMLElement {
  const s = el("span", "swatch");
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  const [w, h] = [64, 26];
  svg.setAttribute("width", String(w));
  svg.setAttribute("height", String(h));
  const pad = Math.max(1, t.border_width) / 2;
  const pts = outline(t.shape, t.corners, w - 2 * pad, h - 2 * pad);
  const shape = document.createElementNS(ns, "polygon");
  shape.setAttribute("points", pts.map((p) => `${(p.x + pad).toFixed(1)},${(p.y + pad).toFixed(1)}`).join(" "));
  shape.setAttribute("fill", t.bg);
  shape.setAttribute("stroke", t.border);
  shape.setAttribute("stroke-width", String(Math.min(t.border_width, 4)));
  shape.setAttribute("stroke-linejoin", "round");
  const text = document.createElementNS(ns, "text");
  for (const [k, v] of Object.entries({ x: w / 2, y: 17, "text-anchor": "middle", "font-size": 12, fill: t.fg })) text.setAttribute(k, String(v));
  text.textContent = "Aa";
  svg.append(shape, text);
  s.append(svg);
  return s;
}

export function edgeSwatch(t: EdgeType, background: string): HTMLElement {
  const s = el("span", "swatch edge");
  s.style.background = background;
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("width", "64");
  svg.setAttribute("height", "26");
  const line = document.createElementNS(ns, "line");
  for (const [k, v] of Object.entries({ x1: 6, y1: 18, x2: 58, y2: 18, stroke: t.color, "stroke-width": t.width, "stroke-linecap": "round" })) line.setAttribute(k, String(v));
  const dash = dashFor(t.line_style, t.width);
  if (dash) line.setAttribute("stroke-dasharray", dash);
  svg.append(line);
  const mark = (kind: string, x: number, dir: 1 | -1) => {
    if (kind === "arrow") {
      const p = document.createElementNS(ns, "polygon");
      p.setAttribute("points", `${x},18 ${x + dir * -8},14 ${x + dir * -8},22`);
      p.setAttribute("fill", t.color);
      svg.append(p);
    } else if (kind === "dot") {
      const c = document.createElementNS(ns, "circle");
      for (const [k, v] of Object.entries({ cx: x - dir * 3, cy: 18, r: 3, fill: t.color })) c.setAttribute(k, String(v));
      svg.append(c);
    } else if (kind === "bar") {
      const l = document.createElementNS(ns, "line");
      for (const [k, v] of Object.entries({ x1: x, y1: 12, x2: x, y2: 24, stroke: t.color, "stroke-width": 2 })) l.setAttribute(k, String(v));
      svg.append(l);
    }
  };
  mark(t.start, 6, -1);
  mark(t.end, 58, 1);
  const text = document.createElementNS(ns, "text");
  for (const [k, v] of Object.entries({ x: 32, y: 11, "text-anchor": "middle", "font-size": 9, fill: t.text_color })) text.setAttribute(k, String(v));
  text.textContent = t.label;
  svg.append(text);
  s.append(svg);
  return s;
}

export function setupTypes(panel: HTMLElement, button: HTMLElement, hooks: TypesHooks) {
  const { graph } = hooks;
  let open = false;
  let shown = "";
  /** The type being edited (a copy), or a new one with no id. */
  let editing: { kind: "node" | "edge"; draft: NodeType | EdgeType; isNew: boolean } | null = null;
  let message = "";

  const toggle = () => {
    open = !open;
    panel.style.display = open ? "block" : "none";
    button.classList.toggle("on", open);
    shown = "";
    update();
  };
  button.addEventListener("click", toggle);
  // Typing in the panel belongs to the panel: Delete, Ctrl+Z and the like must not reach the graph.
  for (const type of ["keydown", "keyup", "keypress"]) panel.addEventListener(type, (e) => e.stopPropagation());

  async function run(ops: object[]) {
    const err = await hooks.send(ops);
    message = err ?? "";
    if (!err) editing = null;
    shown = "";
    update();
  }

  function colorRow(label: string, value: string, set: (c: string) => void, note: () => string): HTMLElement {
    const row = el("label", "row");
    const input = el("input");
    input.type = "color";
    input.value = value;
    const text = el("input", "hex");
    text.value = value;
    text.maxLength = 7;
    const hint = el("small", "hint", note());
    const apply = (c: string | null) => {
      if (!c) return;
      input.value = c;
      text.value = c;
      set(c);
      hint.textContent = note();
      refreshSave();
    };
    input.oninput = () => apply(normalizeColor(input.value));
    text.oninput = () => apply(normalizeColor(text.value));
    row.append(el("span", undefined, label), input, text, hint);
    return row;
  }

  function selectRow(label: string, value: string, options: readonly string[], set: (v: string) => void): HTMLElement {
    const row = el("label", "row");
    const sel = el("select");
    for (const o of options) sel.append(new Option(o, o, false, o === value));
    sel.onchange = () => set(sel.value);
    row.append(el("span", undefined, label), sel);
    return row;
  }

  function numberRow(label: string, value: number, min: number, max: number, set: (v: number) => void): HTMLElement {
    const row = el("label", "row");
    const input = el("input");
    input.type = "number";
    input.min = String(min);
    input.max = String(max);
    input.value = String(value);
    input.onchange = () => {
      const v = Math.min(max, Math.max(min, Math.round(Number(input.value)) || min));
      input.value = String(v);
      set(v);
    };
    row.append(el("span", undefined, label), input, el("small", "hint", `${min}-${max}`));
    return row;
  }

  function textRow(label: string, value: string, set: (v: string) => void, locked = false): HTMLElement {
    const row = el("label", "row");
    const input = el("input");
    input.value = value;
    input.disabled = locked;
    input.oninput = () => {
      set(input.value);
      refreshSave();
    };
    row.append(el("span", undefined, label), input);
    return row;
  }

  let save: HTMLButtonElement | null = null;
  let problems: () => string[] = () => [];
  const problemBox = el("div", "problems");
  function refreshSave() {
    const p = problems();
    problemBox.textContent = p.join(" · ");
    if (save) save.disabled = p.length > 0;
  }

  function ratio(a: string, b: string, need: number): string {
    const r = contrast(a, b);
    return `${r.toFixed(1)}:1 ${r >= need ? "ok" : `too low (needs ${need}:1)`}`;
  }

  function form(background: string): HTMLElement {
    const f = el("div", "form");
    const e = editing!;
    const original = e.isNew ? "" : (e.draft as NodeType | EdgeType).name;
    if (e.kind === "node") {
      const d = e.draft as NodeType;
      f.append(
        el("h4", undefined, e.isNew ? "New node type" : `Edit node type`),
        textRow("name", d.name, (v) => (d.name = v), d.id === DEFAULT_NODE_TYPE),
        selectRow("shape", d.shape, NODE_SHAPES, (v) => (d.shape = v)),
        colorRow("background", d.bg, (c) => (d.bg = c), () => `text ${ratio(d.bg, d.fg, MIN_CONTRAST)}`),
        colorRow("text", d.fg, (c) => (d.fg = c), () => `${ratio(d.bg, d.fg, MIN_CONTRAST)}`),
        colorRow("border", d.border, (c) => (d.border = c), () => ""),
        numberRow("border width", d.border_width, 0, MAX_WIDTH, (v) => (d.border_width = v)),
        selectRow("corners", d.corners, CORNERS, (v) => (d.corners = v)),
      );
      problems = () => {
        const p: string[] = [];
        if (!/^[a-z0-9_.-]{1,64}$/.test(d.name.trim().toLowerCase())) p.push("name: a-z 0-9 _ - .");
        if (contrast(d.bg, d.fg) < MIN_CONTRAST) p.push("text does not contrast enough with the background");
        return p;
      };
    } else {
      const d = e.draft as EdgeType;
      f.append(
        el("h4", undefined, e.isNew ? "New edge type" : "Edit edge type"),
        textRow("name", d.name, (v) => (d.name = v), d.id === DEFAULT_EDGE_TYPE),
        textRow("label", d.label, (v) => (d.label = v)),
        colorRow("line color", d.color, (c) => (d.color = c), () => `on the canvas ${ratio(background, d.color, MIN_LINE_CONTRAST)}`),
        colorRow("text color", d.text_color, (c) => (d.text_color = c), () => `on the canvas ${ratio(background, d.text_color, MIN_CONTRAST)}`),
        numberRow("width", d.width, MIN_EDGE_WIDTH, MAX_WIDTH, (v) => (d.width = v)),
        selectRow("line style", d.line_style, LINE_STYLES, (v) => (d.line_style = v)),
        selectRow("start (source)", d.start, ARROWS, (v) => (d.start = v)),
        selectRow("end (destination)", d.end, ARROWS, (v) => (d.end = v)),
      );
      problems = () => {
        const p: string[] = [];
        if (!/^[a-z0-9_.-]{1,64}$/.test(d.name.trim().toLowerCase())) p.push("name: a-z 0-9 _ - .");
        if (contrast(background, d.color) < MIN_LINE_CONTRAST) p.push("line is too close to the canvas colour");
        if (contrast(background, d.text_color) < MIN_CONTRAST) p.push("text is too close to the canvas colour");
        return p;
      };
    }
    save = el("button", "primary", "Save");
    save.onclick = () => {
      const d = e.draft;
      const name = d.name.trim().toLowerCase();
      const op =
        e.kind === "node"
          ? (() => {
              const n = d as NodeType;
              return { op: "upsert_node_type", name: e.isNew ? name : original, new_name: e.isNew ? undefined : name, shape: n.shape, bg: n.bg, fg: n.fg, border: n.border, border_width: n.border_width, corners: n.corners };
            })()
          : (() => {
              const n = d as EdgeType;
              return { op: "upsert_edge_type", name: e.isNew ? name : original, new_name: e.isNew ? undefined : name, label: n.label.trim(), color: n.color, text_color: n.text_color, width: n.width, line_style: n.line_style, start: n.start, end: n.end };
            })();
      void run([op]);
    };
    const cancel = el("button", undefined, "Cancel");
    cancel.onclick = () => {
      editing = null;
      message = "";
      shown = "";
      update();
    };
    const actions = el("div", "actions", undefined);
    actions.append(save, cancel);
    const id = (e.draft as NodeType | EdgeType).id;
    if (!e.isNew && id !== DEFAULT_NODE_TYPE && id !== DEFAULT_EDGE_TYPE) {
      const del = el("button", "danger", "Delete");
      let armed = false;
      del.onclick = () => {
        if (!armed) {
          armed = true;
          del.textContent = "Really delete? What uses it will look like the default";
          return;
        }
        void run([{ op: e.kind === "node" ? "remove_node_type" : "remove_edge_type", name: original }]);
      };
      actions.append(del);
    }
    f.append(problemBox, actions);
    refreshSave();
    return f;
  }

  function update() {
    if (!open) return;
    const reg = graph.registry();
    const node = hooks.selectedNode();
    const edge = hooks.selectedEdge();
    const sig = JSON.stringify([reg, node, edge, editing?.kind, editing?.isNew, editing && (editing.draft as { id: string }).id, message, graph.nodes.get(node ?? "")?.get("type")]);
    if (sig === shown) return;
    shown = sig;
    panel.replaceChildren();
    panel.append(el("h3", undefined, "Types"));

    // Canvas colour.
    const bg = el("div", "section");
    bg.append(el("h4", undefined, "Canvas"));
    // Laid out like the type rows below, so its button lines up with theirs on the right.
    const bgRow = el("div", "type");
    const bgInput = el("input");
    bgInput.type = "color";
    bgInput.value = reg.background;
    bgInput.disabled = !hooks.isOwner;
    const bgName = el("span", "name", reg.background);
    bgName.style.flex = "1";
    const bgSet = el("button", undefined, "Set canvas colour");
    bgSet.style.marginLeft = "auto";
    bgSet.disabled = !hooks.isOwner;
    bgSet.onclick = async () => {
      message = (await hooks.setBackground(bgInput.value)) ?? "";
      shown = "";
      update();
    };
    bgRow.append(bgInput, bgName, bgSet);
    bg.append(bgRow, el("small", "hint", hooks.isOwner ? "Edge colours that no longer show are recoloured." : "Only the owner can change it."));
    panel.append(bg);

    const usedNode = node ? reg.nodeType(node).id : null;
    const usedEdge = edge ? reg.edgeType(graph.edges().find((x) => x.from === edge.from && x.to === edge.to)?.type ?? "").id : null;

    const list = (kind: "node" | "edge") => {
      const sec = el("div", "section");
      sec.append(el("h4", undefined, kind === "node" ? "Node types" : "Edge types"));
      const types: (NodeType | EdgeType)[] = kind === "node" ? reg.nodeTypes : reg.edgeTypes;
      for (const t of types) {
        const row = el("div", "type");
        row.append(kind === "node" ? nodeSwatch(t as NodeType) : edgeSwatch(t as EdgeType, reg.background), el("span", "name", t.name));
        const target = kind === "node" ? node : edge;
        if (hooks.canEdit && target) {
          const use = el("button", undefined, (kind === "node" ? usedNode : usedEdge) === t.id ? "in use" : kind === "node" ? "use on node" : "use on link");
          use.disabled = (kind === "node" ? usedNode : usedEdge) === t.id;
          use.onclick = () =>
            void run(
              kind === "node"
                ? [{ op: "set_node_type", node, node_type: t.name }]
                : [{ op: "link", from: edge!.from, to: edge!.to, edge_type: t.name }],
            );
          row.append(use);
        }
        if (hooks.canEdit) {
          const edit = el("button", undefined, "edit");
          edit.onclick = () => {
            editing = { kind, draft: { ...t }, isNew: false };
            message = "";
            shown = "";
            update();
          };
          row.append(edit);
        }
        sec.append(row);
      }
      if (hooks.canEdit) {
        const add = el("button", undefined, kind === "node" ? "+ new node type" : "+ new edge type");
        add.onclick = () => {
          const base = kind === "node" ? reg.nodeTypes.find((t) => t.id === DEFAULT_NODE_TYPE)! : reg.edgeTypes.find((t) => t.id === DEFAULT_EDGE_TYPE)!;
          editing = { kind, draft: { ...base, id: "", name: "", ...(kind === "edge" ? { label: "" } : {}) }, isNew: true };
          message = "";
          shown = "";
          update();
        };
        sec.append(add);
      }
      return sec;
    };
    panel.append(list("node"), list("edge"));
    if (editing) panel.append(form(reg.background));
    if (message) panel.append(el("div", "error", message));
    if (!hooks.canEdit) panel.append(el("small", "hint", "You have a viewer key: types can be looked at but not changed."));
  }

  return { update };
}
