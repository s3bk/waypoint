// The graph document, shaped as in DATA_MODEL.md "Document shape": `nodes` is
// a Y.Map<NodeId, Y.Map>, one key per field. `place` is one key holding the
// whole (anchor, offset) pair so it merges as a unit.

import * as Y from "yjs";

import {
  DEFAULT_BACKGROUND,
  DEFAULT_EDGE_TYPE,
  DEFAULT_NODE_TYPE,
  type EdgeType,
  type NodeType,
  normalizeColor,
  readEdgeTypes,
  readNodeTypes,
  readTags,
  type Tag,
} from "./style";

export interface Vec2 {
  x: number;
  y: number;
}

export interface Place {
  anchor: string | null;
  off: Vec2;
}

/** Well-known id of the seeded `link` edge type (waypoint_model::EdgeTypeId::DEFAULT). */
export const DEFAULT_EDGE = "00000000-0000-7000-8000-000000000001";

export interface Edge {
  key: string;
  from: string;
  to: string;
  type: string;
  /** Set by the server: this edge joins a node to itself and is set aside. */
  conflicted: boolean;
}

/** The tags (known ones only) that any of these document entries carries, in registry order. */
function tagsOn(entries: Y.Map<unknown>[], known: Map<string, Tag>): Tag[] {
  const on = new Set<string>();
  for (const e of entries) for (const [k, v] of e.entries()) if (v === true && k.startsWith("tag:")) on.add(k.slice(4));
  return [...known.values()].filter((t) => on.has(t.id));
}

export class Graph {
  readonly doc = new Y.Doc();
  readonly nodes = this.doc.getMap<Y.Map<unknown>>("nodes");
  readonly edgeMap = this.doc.getMap<Y.Map<unknown>>("edges");
  /** Document-level settings; `title` is written only by the server, on the owner's request. */
  readonly meta = this.doc.getMap<string>("meta");
  /** Type registries: `id -> Y.Map` of fields; the server writes them (see style.ts). */
  readonly nodeTypeMap = this.doc.getMap<Y.Map<unknown>>("node_types");
  readonly edgeTypeMap = this.doc.getMap<Y.Map<unknown>>("edge_types");
  /** The tag registry; nodes and links point at its entries by id with `tag:<id>` fields. */
  readonly tagMap = this.doc.getMap<Y.Map<unknown>>("tags");
  /** Viewers watch only: the server would ignore their edits, so they are never made. */
  readOnly = false;

  private write(change: () => void) {
    if (!this.readOnly) this.doc.transact(change);
  }

  createNode(id: string, title: string, place: Place) {
    this.write(() => {
      const node = new Y.Map<unknown>();
      node.set("title", title);
      node.set("place", JSON.stringify(place));
      node.set("deleted", false);
      this.nodes.set(id, node);
    });
  }

  /** Live nodes' places, in the JSON shape the layout crate expects. */
  places(): Record<string, Place> {
    const out: Record<string, Place> = {};
    this.nodes.forEach((node, id) => {
      if (node.get("deleted") === true) return;
      const place = node.get("place");
      if (typeof place === "string") out[id] = JSON.parse(place) as Place;
    });
    return out;
  }

  /**
   * `from` is the source and `to` the destination. An edge is identified by its ordered pair, so the key
   * is derived from the two ends; repeating it is a no-op and a removed edge comes back.
   */
  createEdge(from: string, to: string, type: string) {
    const key = `${from}>${to}`;
    const existing = this.edgeMap.get(key);
    if (existing) {
      existing.set("deleted", false);
      existing.set("conflicted", false); // the server re-checks it
      return;
    }
    const edge = new Y.Map<unknown>();
    edge.set("from", from);
    edge.set("to", to);
    edge.set("type", type);
    edge.set("deleted", false);
    this.edgeMap.set(key, edge);
  }

  /**
   * Remove an edge (soft delete) and apply the anchor repairs it makes
   * necessary, in one transaction. `key` is the edge's `"from>to"` pair key.
   * Every entry for that pair is marked, including ones stored under the older
   * `"from>to:type"` spelling.
   */
  removeEdge(key: string, changes: [string, Place][]) {
    const [from, to] = key.split(">");
    this.write(() => {
      this.edgeMap.forEach((e) => {
        if (e.get("from") === from && e.get("to") === to) e.set("deleted", true);
      });
      for (const [id, place] of changes) this.nodes.get(id)?.set("place", JSON.stringify(place));
    });
  }

  /**
   * Delete a node (soft delete, so it can come back) in one transaction: its
   * links are hidden, `bridges` (`[from, to, type]`) are added so chains
   * through it survive, and `changes` re-anchor nodes that hung off it.
   */
  deleteNode(id: string, bridges: [string, string, string][], changes: [string, Place][]) {
    this.write(() => {
      for (const [from, to, type] of bridges) this.createEdge(from, to, type);
      this.edgeMap.forEach((e) => {
        if (e.get("from") === id || e.get("to") === id) e.set("deleted", true);
      });
      for (const [nodeId, place] of changes) this.nodes.get(nodeId)?.set("place", JSON.stringify(place));
      this.nodes.get(id)?.set("deleted", true);
    });
  }

  /** Live edges as `[from, to]` pairs, the shape the layout crate takes. Conflicted edges still join their ends. */
  edgePairs(except?: string): [string, string][] {
    return this.edges().filter((e) => e.key !== except).map((e) => [e.from, e.to]);
  }

  /**
   * Add a node and apply the layout changes that make room for it, as one
   * transaction. `changes` comes from the layout crate and includes the new
   * node's own place. `link` is an optional `[from, to]` edge ("from is
   * on to") created with it.
   */
  addNode(id: string, title: string, changes: [string, Place][], link?: [string, string]) {
    this.write(() => {
      for (const [nodeId, place] of changes) {
        if (nodeId === id) this.createNode(id, title, place);
        else this.nodes.get(nodeId)?.set("place", JSON.stringify(place));
      }
      if (link) this.createEdge(link[0], link[1], DEFAULT_EDGE);
    });
  }

  /** Write several places in one transaction, e.g. the result of a drag (one undo step). */
  applyPlaces(changes: [string, Place][]) {
    this.write(() => {
      for (const [id, place] of changes) this.nodes.get(id)?.set("place", JSON.stringify(place));
    });
  }

  /**
   * Live edges, one per ordered pair, whose ends both exist and are live. An
   * edge is identified by its pair, so entries under different key spellings
   * for the same pair count once (and are live if any of them is).
   */
  edges(): Edge[] {
    const byPair = new Map<string, Edge>();
    const live = (id: unknown) => typeof id === "string" && this.nodes.get(id)?.get("deleted") === false;
    this.edgeMap.forEach((e, entryKey) => {
      const from = e.get("from");
      const to = e.get("to");
      if (e.get("deleted") === true || !live(from) || !live(to)) return;
      const key = `${from as string}>${to as string}`;
      // The entry stored under the canonical key decides the type.
      if (!byPair.has(key) || entryKey === key) byPair.set(key, { key, from: from as string, to: to as string, type: String(e.get("type")), conflicted: e.get("conflicted") === true && from === to });
    });
    return [...byPair.values()];
  }

  /** The canvas colour, set by the owner. */
  background(): string {
    return normalizeColor(this.meta.get("background")) ?? DEFAULT_BACKGROUND;
  }

  /** The registries as they stand, with lookups that fall back to the default type. Read once per draw. */
  registry() {
    const fields = (m: Y.Map<Y.Map<unknown>>) => [...m.entries()].map(([id, e]) => [id, new Map(e.entries())] as [string, Map<string, unknown>]);
    const background = this.background();
    const nodeTypes = readNodeTypes(fields(this.nodeTypeMap));
    const edgeTypes = readEdgeTypes(fields(this.edgeTypeMap), background);
    const tags = readTags(fields(this.tagMap));
    const tagById = new Map(tags.map((t) => [t.id, t]));
    const nodeById = new Map(nodeTypes.map((t) => [t.id, t]));
    const edgeById = new Map(edgeTypes.map((t) => [t.id, t]));
    return {
      background,
      nodeTypes,
      edgeTypes,
      tags,
      /** The tags on node `id` that exist, in registry order. */
      nodeTags: (id: string): Tag[] => tagsOn(this.nodes.get(id) ? [this.nodes.get(id)!] : [], tagById),
      /** The tags on the link from `from` to `to`. */
      edgeTags: (from: string, to: string): Tag[] => {
        const entries: Y.Map<unknown>[] = [];
        this.edgeMap.forEach((e) => {
          if (e.get("from") === from && e.get("to") === to && e.get("deleted") !== true) entries.push(e);
        });
        return tagsOn(entries, tagById);
      },
      /** The type of node `id`; a node with no type, or a removed one, has the default. */
      nodeType: (id: string): NodeType => {
        const t = this.nodes.get(id)?.get("type");
        return (typeof t === "string" ? nodeById.get(t) : undefined) ?? nodeById.get(DEFAULT_NODE_TYPE)!;
      },
      edgeType: (typeId: string): EdgeType => edgeById.get(typeId) ?? edgeById.get(DEFAULT_EDGE_TYPE)!,
    };
  }

  /** The graph's name, set by its owner; empty until then. */
  graphTitle(): string {
    return this.meta.get("title") ?? "";
  }

  /** The node's shared note: one Y.Text per node, created by the server, edited live by everyone. */
  noteText(id: string): Y.Text | undefined {
    const t = this.nodes.get(id)?.get("note");
    return t instanceof Y.Text ? t : undefined;
  }

  note(id: string): string {
    return this.noteText(id)?.toString() ?? "";
  }

  /** The short, human-friendly key the server assigned (shown as #12); undefined until it has. */
  key(id: string): number | undefined {
    const k = this.nodes.get(id)?.get("key");
    return typeof k === "number" ? k : undefined;
  }

  /** Rename a node. A title is one value, so concurrent renames resolve to the later one. */
  setTitle(id: string, title: string) {
    this.nodes.get(id)?.set("title", title);
  }

  title(id: string): string {
    return String(this.nodes.get(id)?.get("title") ?? "");
  }
}
