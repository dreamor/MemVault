import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// https://vite.dev/config/
export default defineConfig({
  plugins: [react()],
  server: {
    port: 1420,
    // `npm run dev` talks to the local REST backend without CORS by proxying
    // API paths to the running `memvault-mcp --transport http` instance.
    proxy: {
      "/api": "http://127.0.0.1:3777",
      "/health": "http://127.0.0.1:3777",
      "/metrics": "http://127.0.0.1:3777",
    },
  },
});