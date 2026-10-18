import { defineConfig } from "vite";

// The Rust server runs separately; forward API calls to it. The e2e tests point
// this at a throwaway server through WAYPOINT_SERVER (host:port).
const server = process.env.WAYPOINT_SERVER ?? "127.0.0.1:3000";

export default defineConfig({
  server: {
    proxy: {
      "/api": `http://${server}`,
      "/ws": { target: `ws://${server}`, ws: true },
    },
  },
});
