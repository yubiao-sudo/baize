import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 端口必须与 tauri.conf.json 的 devUrl 一致
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    // 显式绑 IPv4：Node 17+ 默认把 localhost 解析成 ::1(IPv6)，WebView2 走 127.0.0.1 会连不上导致白屏
    host: "127.0.0.1",
    port: 1421,
    strictPort: true,
  },
  build: {
    target: "es2021",
    chunkSizeWarningLimit: 1500,
    rollupOptions: {
      output: {
        // 把重型第三方库拆成独立 chunk，减小主包体积、改善 Webview 解析与缓存
        manualChunks(id: string) {
          if (!id.includes("node_modules")) return undefined;
          if (id.includes("/three/")) return "vendor-three";
          if (id.includes("/d3-") || id.includes("/d3/")) return "vendor-d3";
          if (id.includes("/@xterm/")) return "vendor-xterm";
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
