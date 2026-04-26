import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import path from 'path';

const host = process.env.TAURI_DEV_HOST;

// https://vitejs.dev/config/
export default defineConfig(async () => ({
  plugins: [svelte({
    preprocess: vitePreprocess()
  })],
  
  resolve: {
    alias: {
      '$lib': path.resolve('./src/lib')
    }
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1421,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1422,
        }
      : undefined,
    watch: {
      // 3. tell vite to ignore watching paths it doesn't need to react to.
      //    src-tauri churns on every Rust rebuild; .claude/docs/dist/target
      //    are agent state and build artifacts, never frontend sources.
      ignored: [
        "**/src-tauri/**",
        "**/.claude/**",
        "**/docs/**",
        "**/dist/**",
        "**/target/**",
        "**/scripts/**",
      ],
    },
  },
}));