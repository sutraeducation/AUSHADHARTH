import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import { VitePWA } from "vite-plugin-pwa";

export default defineConfig({
  plugins: [
    react(),
    VitePWA({
      strategies: "injectManifest",
      srcDir: "src",
      filename: "service-worker.ts",
      injectRegister: "auto",
      registerType: "prompt",
      includeAssets: ["icons/icon.svg"],
      manifest: {
        name: "AUSHADHARTH",
        short_name: "AUSHADHARTH",
        description: "Pharmacy. Inventory. Accounts.",
        theme_color: "#12372a",
        background_color: "#f4f7f5",
        display: "standalone",
        start_url: "/",
        scope: "/",
        icons: [
          {
            src: "/icons/icon.svg",
            sizes: "any",
            type: "image/svg+xml",
            purpose: "any maskable"
          }
        ]
      },
      injectManifest: {
        globPatterns: ["**/*.{js,css,html,svg}"]
      },
      devOptions: { enabled: false }
    })
  ],
  server: {
    proxy: {
      "/api": "http://127.0.0.1:47831"
    }
  },
  test: {
    environment: "jsdom",
    setupFiles: "./src/test/setup.ts",
    css: true
  }
});
