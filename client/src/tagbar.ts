// The side bar: a column on the left, shown while a node or link is selected. It lists the types the
// selection can have (the current one marked) and every tag (marked when the selection carries it).
// A click sets the type, or puts the tag on or takes it off.

import type { Graph } from "./graph";
import type { EdgeType, NodeType } from "./style";
import { edgeSwatch, nodeSwatch, tagChip } from "./types";

export interface TagBarHooks {
  graph: Graph;
  canEdit: boolean;
  send: (ops: object[]) => Promise<string | null>;
  selectedNode: () => string | null;
  selectedEdge: () => { from: string; to: string } | null;
  /** Called when the bar appears (it was hidden). */
  onShow?: () => void;
}

export function setupTagBar(root: HTMLElement, hooks: TagBarHooks) {
  const { graph } = hooks;
  let shown = "";
  let message = "";

  function update() {
    const reg = graph.registry();
    const node = hooks.selectedNode();
    const edge = node ? null : hooks.selectedEdge();
    const target = node ?? edge;
    if (!target) {
      root.style.display = "none";
      shown = "";
      return;
    }
    const has = new Set((node ? reg.nodeTags(node) : reg.edgeTags(edge!.from, edge!.to)).map((t) => t.id));
    const currentType = node ? reg.nodeType(node).id : reg.edgeType(graph.edges().find((x) => x.from === edge!.from && x.to === edge!.to)?.type ?? "").id;
    const sig = JSON.stringify([reg, node, edge, [...has], currentType, message]);
    if (root.style.display !== "block") hooks.onShow?.();
    root.style.display = "block";
    if (sig === shown) return;
    shown = sig;
    root.replaceChildren();
    const h = document.createElement("h3");
    h.textContent = node ? "Node type" : "Link type";
    root.append(h);
    const send = async (ops: object[]) => {
      message = (await hooks.send(ops)) ?? "";
      shown = "";
      update();
    };
    const entry = (swatch: HTMLElement, text: string, on: boolean, click: () => void) => {
      const b = document.createElement("button");
      b.type = "button";
      b.className = on ? "tag on" : "tag";
      b.disabled = !hooks.canEdit;
      b.setAttribute("aria-pressed", String(on));
      const name = document.createElement("span");
      name.className = "name";
      name.textContent = text;
      const mark = document.createElement("span");
      mark.textContent = on ? "✓" : "";
      b.append(swatch, name, mark);
      b.onclick = click;
      root.append(b);
    };
    const types = node ? reg.nodeTypes : reg.edgeTypes;
    for (const t of types) {
      entry(node ? nodeSwatch(t as NodeType) : edgeSwatch(t as EdgeType, reg.background), t.name, t.id === currentType, () =>
        void send(node ? [{ op: "set_node_type", node, node_type: t.name }] : [{ op: "link", from: edge!.from, to: edge!.to, edge_type: t.name }]),
      );
    }
    if (reg.tags.length) {
      const h2 = document.createElement("h3");
      h2.textContent = "Tags";
      root.append(h2);
    }
    for (const t of reg.tags) {
      const on = has.has(t.id);
      const key = on ? "remove" : "add";
      entry(tagChip(t, reg.background), t.name, on, () =>
        void send(node ? [{ op: "set_tags", node, [key]: [t.name] }] : [{ op: "set_tags", from: edge!.from, to: edge!.to, [key]: [t.name] }]),
      );
    }
    if (message) {
      const e = document.createElement("div");
      e.className = "error";
      e.textContent = message;
      root.append(e);
    }
  }

  return { update };
}
