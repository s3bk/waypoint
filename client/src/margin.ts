// The margin: a DOM column beside the canvas (text stays crisp and editable).
// It is a text editor for the selected node's note: one shared text per node,
// edited live by everyone through Yjs (character-level merge), not through
// batches. Below it, the notes of direct neighbours, read-only.

import * as Y from "yjs";
import type { Graph } from "./graph";

/** Neighbours shown before "+N more". */
const NEIGHBOURS_SHOWN = 3;

export interface MarginHooks {
  graph: Graph;
  canEdit: boolean;
  select: (id: string) => void;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string, text?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

/** Where index `i` of the old text is after `delta` was applied. */
function mapIndex(delta: { retain?: number; insert?: string | object; delete?: number }[], i: number): number {
  let old = 0; // position in the old text
  let now = 0; // position in the new text
  for (const op of delta) {
    if (op.retain !== undefined) {
      if (i <= old + op.retain) return now + (i - old);
      old += op.retain;
      now += op.retain;
    } else if (op.insert !== undefined) {
      const n = typeof op.insert === "string" ? op.insert.length : 1;
      if (i === old) return now; // text added at the caret lands after it, as in any editor
      now += n;
    } else if (op.delete !== undefined) {
      if (i <= old + op.delete) return now;
      old += op.delete;
    }
  }
  return now + (i - old);
}

export function setupMargin(root: HTMLElement, hooks: MarginHooks) {
  const { graph } = hooks;
  let selected: string | null = null;
  let expanded = false; // "+N more" clicked, for the current selection
  let shown = ""; // what the non-editor part was drawn from
  let unbind: (() => void) | null = null;

  const label = (id: string) => {
    const k = graph.key(id);
    return `${k !== undefined ? `#${k} ` : ""}${graph.title(id) || "untitled"}`;
  };

  /** Text offset of a (node, offset) point inside `box`. */
  function offsetOf(box: HTMLElement, node: Node, offset: number): number {
    const r = document.createRange();
    r.selectNodeContents(box);
    r.setEnd(node, offset);
    return r.toString().length;
  }

  /** The caret or selection as text offsets, or null when it is not in `box`. */
  function readSelection(box: HTMLElement): [number, number] | null {
    const s = getSelection();
    if (!s || !s.anchorNode || !s.focusNode || !box.contains(s.anchorNode) || !box.contains(s.focusNode)) return null;
    const a = offsetOf(box, s.anchorNode, s.anchorOffset);
    const f = offsetOf(box, s.focusNode, s.focusOffset);
    return [Math.min(a, f), Math.max(a, f)];
  }

  function writeSelection(box: HTMLElement, from: number, to: number) {
    const point = (index: number): [Node, number] => {
      const walker = document.createTreeWalker(box, NodeFilter.SHOW_TEXT);
      let left = index;
      for (let n = walker.nextNode(); n; n = walker.nextNode()) {
        const len = n.textContent!.length;
        if (left <= len) return [n, left];
        left -= len;
      }
      return [box, box.childNodes.length];
    };
    const r = document.createRange();
    r.setStart(...point(from));
    r.setEnd(...point(to));
    const s = getSelection()!;
    s.removeAllRanges();
    s.addRange(r);
  }

  /** Make `box` show and edit `text`: typing becomes a minimal Yjs edit; remote edits update the box and keep the cursor. */
  function bind(box: HTMLElement, text: Y.Text): () => void {
    box.textContent = text.toString();
    const onInput = () => {
      const was = text.toString();
      const now = box.textContent ?? "";
      let start = 0;
      while (start < was.length && start < now.length && was[start] === now[start]) start++;
      let end = 0;
      while (end < was.length - start && end < now.length - start && was[was.length - 1 - end] === now[now.length - 1 - end]) end++;
      graph.doc.transact(() => {
        if (was.length - start - end > 0) text.delete(start, was.length - start - end);
        const added = now.slice(start, now.length - end);
        if (added) text.insert(start, added);
      }, box); // origin: this box, so its own edit is not echoed back
    };
    const onRemote = (event: Y.YTextEvent) => {
      if (event.transaction.origin === box) return;
      const sel = readSelection(box);
      box.textContent = text.toString();
      if (sel) writeSelection(box, mapIndex(event.delta, sel[0]), mapIndex(event.delta, sel[1]));
    };
    box.addEventListener("input", onInput);
    text.observe(onRemote);
    return () => {
      box.removeEventListener("input", onInput);
      text.unobserve(onRemote);
    };
  }

  function drawEditor(sel: string) {
    unbind?.();
    unbind = null;
    root.replaceChildren();
    root.append(el("h3"));
    const text = graph.noteText(sel);
    const box = el("div", "note-editor");
    box.spellcheck = false;
    // Plain text only: no pasted markup, and Enter inserts "\n".
    box.contentEditable = hooks.canEdit && text ? "plaintext-only" : "false";
    box.dataset.placeholder = hooks.canEdit ? "Notes for this node. Everyone sees edits as you type." : "No note.";
    if (text) unbind = bind(box, text);
    root.append(box, el("div", "neighbours"));
  }

  function drawAround(sel: string) {
    root.querySelector("h3")!.textContent = label(sel);
    const holder = root.querySelector(".neighbours")!;
    holder.replaceChildren();
    const edges = graph.edges();
    const around: { id: string; rel: string }[] = [
      ...edges.filter((e) => e.from === sel).map((e) => ({ id: e.to, rel: "depends on" })),
      ...edges.filter((e) => e.to === sel).map((e) => ({ id: e.from, rel: "blocks" })),
    ].filter((n) => graph.note(n.id).trim());
    const visible = expanded ? around : around.slice(0, NEIGHBOURS_SHOWN);
    for (const { id, rel } of visible) {
      const sec = el("section", "neighbour");
      const h = el("h4");
      const link = el("a", undefined, label(id));
      link.addEventListener("click", () => hooks.select(id));
      h.append(el("span", "rel", rel), " ", link);
      sec.append(h, el("div", "note-body", graph.note(id)));
      holder.append(sec);
    }
    if (around.length > visible.length) {
      const more = el("button", "more", `+${around.length - visible.length} more neighbours`);
      more.addEventListener("click", () => {
        expanded = true;
        shown = "";
        update(selected);
      });
      holder.append(more);
    }
  }

  /** Call whenever the document or the selection may have changed. */
  function update(sel: string | null) {
    if (sel !== selected) {
      expanded = false;
      shown = "";
      selected = sel;
      unbind?.();
      unbind = null;
      root.replaceChildren();
    }
    root.style.display = sel ? "flex" : "none";
    if (!sel) return;
    if (!root.querySelector(".note-editor") || (graph.noteText(sel) && !unbind && hooks.canEdit)) {
      drawEditor(sel);
      shown = ""; // the heading and neighbours are drawn below
    }
    const edges = graph.edges();
    const ids = [sel, ...edges.filter((e) => e.from === sel).map((e) => e.to), ...edges.filter((e) => e.to === sel).map((e) => e.from)];
    const sig = JSON.stringify([expanded, ids.map((i) => [i, label(i), i === sel ? "" : graph.note(i)])]);
    if (sig === shown) return;
    shown = sig;
    drawAround(sel);
  }

  return { update };
}
