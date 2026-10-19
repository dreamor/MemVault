import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The auto-managed REST backend writes its active port to
// <memvault-home>/mcp.port (`memvault-cli server start --auto-port` may move
// off the default when 3777 is taken). Bubble it into the dev proxy target so
// `npm run dev` keeps working even when the server landed on another port.
function memvaultPort(): number {
  try {
    const home = process.env.MEMVAULT_HOME ?? join(homedir(), ".memvault");
    const raw = readFileSync(join(home, "mcp.port"), "utf8").trim();
    const port = Number(raw);
    if (Number.isInteger(port) && port > 0 && port < 65536) {
      return port;
    }
  } catch {
    // No port file yet — fall through to the default.
  }
  return 3777;
}

const target = `http://127.0.0.1:${memvaultPort()}`;

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    port: 1420,
    // `npm run dev` talks to the local REST backend without CORS by proxying
    // API paths to the running `memvault-mcp --transport http` instance.
    proxy: {
      "/api": target,
      "/health": target,
      "/metrics": target,
    },
  },
});
