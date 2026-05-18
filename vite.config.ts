import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: {
    port: 1421,
    strictPort: true
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "es2020",
    minify: false
  }
});

