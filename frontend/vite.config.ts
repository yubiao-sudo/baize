import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 端口保持 1422，供 aurora 前端独立开发使用（与桌面版旧前端 1421 完全隔离）
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1422,
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