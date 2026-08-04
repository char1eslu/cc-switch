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
  // 生产构建时把 console.log / console.debug 标记为无副作用，minify 阶段移除；
  // 保留 console.error / console.warn 以便排查线上问题。dev（serve）不受影响。
  esbuild:
    command === "build"
      ? { pure: ["console.log", "console.debug"] }
      : undefined,
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    chunkSizeWarningLimit: 1000,
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
