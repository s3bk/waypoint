// Runs the layout search off the main thread: a big graph takes seconds, and the page must stay usable
// (and cancellable: the page just terminates this worker).
import init, { arrange } from "./wasm/waypoint_layout.js";

export interface LayoutRequest {
  method: string;
  /** JSON `[[id, type number], ...]`. */
  nodes: string;
  /** JSON `[[from, to, label width in px (0: no label)], ...]`. */
  edges: string;
  /** JSON `{ id: {x, y} }`: where the nodes are now. */
  current: string;
  aspect: number;
  /** The area the force-directed methods must stay inside, in world units (0: no limit). */
  boundsW: number;
  boundsH: number;
}

/** `result` is JSON `{ positions: { id: {x, y} }, defects: number }`. */
export type LayoutReply = { result: string } | { error: string };

const ready = init();

self.onmessage = async (e: MessageEvent<LayoutRequest>) => {
  const r = e.data;
  try {
    await ready;
    const reply: LayoutReply = { result: arrange(r.method, r.nodes, r.edges, r.current, r.aspect, r.boundsW, r.boundsH) };
    self.postMessage(reply);
  } catch (err) {
    const reply: LayoutReply = { error: String(err) };
    self.postMessage(reply);
  }
};
