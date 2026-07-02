import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// The Deno backend runs on :4321. Proxy the REST + SSE surface to it in dev so
// the UI can talk to the live server without CORS or a second origin.
export default defineConfig({
  plugins: [react()],
  server: {
    proxy: {
      "/health": "http://localhost:4321",
      "/sessions": "http://localhost:4321",
      "/events": {
        target: "http://localhost:4321",
        // SSE is a long-lived stream — don't let the proxy buffer or time it out.
        changeOrigin: true,
      },
    },
  },
});
