// The layout tool: pick a way to arrange the whole graph, see the result on the canvas without sending
// anything, then accept (one batch, one undo step) or cancel. The search runs in a worker.

import type { LayoutReply, LayoutRequest } from "./layoutWorker";
import type { Vec2 } from "./graph";

export interface LayoutInput {
  /** `[node id, type number]`: nodes of one type get the same number. */
  nodes: [string, number][];
  /** `[from, to, label width in px]`; 0 for a link without a label. */
  edges: [string, string, number][];
  /** Where the nodes are now (top-left corners). */
  current: Record<string, Vec2>;
  /** Wanted width / height of the drawing. */
  aspect: number;
  /** Area the force-directed layouts stay inside (world units). */
  bounds: [number, number];
}

export interface LayoutHooks {
  canEdit: boolean;
  input(): LayoutInput;
  /** Show these positions (top-left corners, drawing at the origin) on the canvas instead of the stored ones. */
  show(positions: Record<string, Vec2>): void;
  /** Stop showing a preview. */
  clear(): void;
  /** Write the shown positions to the graph. Resolves to an error message, or null when applied. */
  accept(): Promise<string | null>;
}

const METHODS: { id: string; name: string; about: string }[] = [
  { id: "clustered", name: "Group by type", about: "Nodes of one type sit together; no overlaps, links clear of nodes, room for link labels." },
  { id: "force", name: "Force-directed", about: "Linked nodes pull together, others push apart. Types are ignored." },
  { id: "layered", name: "Layers, left to right", about: "Dependencies on the left, one column per step. Fast; long on deep graphs." },
  { id: "polish", name: "Clean up", about: "Keeps the arrangement and moves only what is in the way." },
];

export function setupLayout(panel: HTMLElement, button: HTMLElement, hooks: LayoutHooks) {
  let open = false;
  let worker: Worker | null = null;
  /** The method being computed (`computing`) or shown (`shown`). */
  let computing: string | null = null;
  let shown: string | null = null;
  /** Rule breaks left in the shown preview (a tight area can make them unavoidable). */
  let defects = 0;
  let message = "";

  const stop = () => {
    worker?.terminate();
    worker = null;
    computing = null;
  };
  const discard = () => {
    stop();
    if (shown) hooks.clear();
    shown = null;
    message = "";
  };

  const toggle = () => {
    open = !open;
    panel.style.display = open ? "block" : "none";
    button.classList.toggle("on", open);
    if (open) draw();
    else discard();
  };
  button.addEventListener("click", toggle);
  panel.addEventListener("keydown", (e) => e.stopPropagation());
  // Escape drops a preview (or closes the tool when there is none).
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && open) toggle();
  });

  function run(method: string) {
    stop();
    if (shown) hooks.clear();
    shown = null;
    computing = method;
    message = "";
    draw();
    const input = hooks.input();
    if (input.nodes.length === 0) {
      computing = null;
      message = "The graph is empty.";
      draw();
      return;
    }
    const w = new Worker(new URL("./layoutWorker.ts", import.meta.url), { type: "module" });
    worker = w;
    w.onmessage = (e: MessageEvent<LayoutReply>) => {
      if (worker !== w) return;
      stop();
      if ("error" in e.data) {
        message = `Layout failed: ${e.data.error}`;
      } else {
        const result = JSON.parse(e.data.result) as { positions: Record<string, Vec2>; defects: number };
        hooks.show(result.positions);
        defects = result.defects;
        shown = method;
      }
      draw();
    };
    w.onerror = (e) => {
      if (worker !== w) return;
      stop();
      message = `Layout failed: ${e.message}`;
      draw();
    };
    const request: LayoutRequest = {
      method,
      nodes: JSON.stringify(input.nodes),
      edges: JSON.stringify(input.edges),
      current: JSON.stringify(input.current),
      aspect: input.aspect,
      boundsW: input.bounds[0],
      boundsH: input.bounds[1],
    };
    w.postMessage(request);
  }

  async function accept() {
    const err = await hooks.accept();
    if (err) {
      message = err;
      draw();
      return;
    }
    // The graph now has the layout: nothing to cancel any more.
    shown = null;
    message = "Applied. Ctrl+Z undoes it.";
    draw();
  }

  function draw() {
    const el = <K extends keyof HTMLElementTagNameMap>(tag: K, className?: string, text?: string) => {
      const e = document.createElement(tag);
      if (className) e.className = className;
      if (text !== undefined) e.textContent = text;
      return e;
    };
    const title = el("h3", undefined, "Layout");
    const list = el("div", "methods");
    for (const m of METHODS) {
      const b = el("button", m.id === (computing ?? shown) ? "pick on" : "pick");
      b.id = `layout-${m.id}`;
      b.append(el("b", undefined, m.name), el("span", undefined, m.about));
      b.onclick = () => run(m.id);
      list.append(b);
    }
    const status = el("div", "status");
    if (computing) {
      status.append(el("span", undefined, "Computing…"));
      const cancel = el("button", undefined, "Cancel");
      cancel.id = "layout-cancel";
      cancel.onclick = () => {
        stop();
        draw();
      };
      status.append(cancel);
    } else if (shown) {
      const name = METHODS.find((m) => m.id === shown)?.name ?? shown;
      status.append(el("span", undefined, `Previewing “${name}”. Nothing is sent until you accept.`));
      if (defects > 0 && (shown === "clustered" || shown === "force")) {
        status.append(el("small", "hint", `The screen area is too tight to avoid ${defects} overlap${defects === 1 ? "" : "s"} or crowded spot${defects === 1 ? "" : "s"}. Zoom out first for more room.`));
      }
      const row = el("div", "actions");
      const ok = el("button", "primary", "Accept");
      ok.id = "layout-accept";
      ok.disabled = !hooks.canEdit;
      ok.onclick = () => void accept();
      const no = el("button", undefined, "Cancel");
      no.id = "layout-discard";
      no.onclick = () => {
        discard();
        draw();
      };
      row.append(ok, no);
      status.append(row);
      if (!hooks.canEdit) status.append(el("small", "hint", "You have a viewer key: layouts can be previewed but not applied."));
    } else if (message) {
      status.append(el("span", message.startsWith("Applied") ? undefined : "error", message));
    }
    panel.replaceChildren(title, list, status);
  }

  return { close: () => open && toggle() };
}
