// What is on the canvas, as plain data: the renderer builds it, and the SVG/PNG
// export reads it, so a file shows exactly what the screen shows.

import type { Vec2 } from "./graph";
import type { EdgeType, NodeType } from "./style";

// Must match waypoint_model::NODE_WIDTH / NODE_HEIGHT.
export const NODE_W = 160;
export const NODE_H = 56;

export interface SceneNode {
  id: string;
  /** Top-left corner, world coordinates. */
  x: number;
  y: number;
  type: NodeType;
  /** The title as it is drawn: already broken into lines. */
  lines: string[];
  /** No title yet: drawn as a faint "untitled". */
  untitled: boolean;
  key?: number;
}

export interface SceneEdge {
  /** Where the line leaves the source and where it reaches the destination. */
  a: Vec2;
  b: Vec2;
  type: EdgeType;
  conflicted: boolean;
  /** Where the type's label is drawn, if there was room for it. */
  label: { x: number; y: number; angle: number } | null;
}

export interface Scene {
  background: string;
  nodes: SceneNode[];
  edges: SceneEdge[];
}
