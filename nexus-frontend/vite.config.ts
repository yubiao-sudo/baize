import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 端口 1423 —— Nexus 棱镜前端（与桌面版 1421 / aurora 1422 完全隔离）
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1423,
    strictPort: true,
  },
  build: {
    target: "es2021",
    chunkSizeWarningLimit: 1200,
    rollupOptions: {
      output: {
        manualChunks(id: string) {
          if (!id.includes("node_modules")) return undefined;
          if (id.includes("/react/") || id.includes("/react-dom/") || id.includes("/scheduler/")) {
            return "vendor-react";
          }
          if (id.includes("/marked/")) return "vendor-markdown";
          if (id.includes("/zustand/")) return "vendor-state";
          return "vendor";
        },
      },
    },
  },
});