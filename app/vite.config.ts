import { defineConfig } from "vite";

// Tauri 要求固定端口，且不能清屏（会吞掉 tauri 的输出）
export default defineConfig({
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    // Tauri 用的 WebView：macOS 上是 WKWebView
    target: "safari15",
    minify: "esbuild",
    sourcemap: false,
  },
});
