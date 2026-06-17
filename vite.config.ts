import path from "node:path";
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { codeInspectorPlugin } from "code-inspector-plugin";

export default defineConfig(({ command }) => ({
  root: "src",
  plugins: [
    command === "serve" &&
      codeInspectorPlugin({
        bundler: "vite",
      }),
    react(),
  ].filter(Boolean),
  base: "./",
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    chunkSizeWarningLimit: 4096,
    rollupOptions: {
      output: {
        manualChunks: {
          vendor: ["react", "react-dom"],
          codemirror: [
            "codemirror",
            "@codemirror/lang-javascript",
            "@codemirror/lang-json",
            "@codemirror/lang-markdown",
            "@codemirror/lint",
            "@codemirror/state",
            "@codemirror/theme-one-dark",
            "@codemirror/view",
          ],
          charts: ["recharts"],
          markdown: ["react-markdown", "remark-gfm"],
        },
      },
    },
  },
  server: {
    port: 3000,
    strictPort: true,
  },
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  clearScreen: false,
  envPrefix: ["VITE_", "TAURI_"],
}));
