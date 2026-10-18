// The one camera transform. The canvas and the DOM overlays (notes, command
// line) both derive screen positions from this, so they cannot disagree.

export interface Vec2 {
  x: number;
  y: number;
}

export class Camera {
  // World coordinate shown at the screen origin, and scale.
  x = 0;
  y = 0;
  zoom = 1;

  toScreen(w: Vec2): Vec2 {
    return { x: (w.x - this.x) * this.zoom, y: (w.y - this.y) * this.zoom };
  }

  toWorld(s: Vec2): Vec2 {
    return { x: s.x / this.zoom + this.x, y: s.y / this.zoom + this.y };
  }

  panBy(dxScreen: number, dyScreen: number) {
    this.x -= dxScreen / this.zoom;
    this.y -= dyScreen / this.zoom;
  }

  /** Zoom keeping the world point under `screen` fixed. */
  zoomAt(screen: Vec2, factor: number) {
    const before = this.toWorld(screen);
    this.zoom = Math.min(4, Math.max(0.1, this.zoom * factor));
    const after = this.toWorld(screen);
    this.x += before.x - after.x;
    this.y += before.y - after.y;
  }
}
