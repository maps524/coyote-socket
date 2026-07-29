import { defineConfig } from 'vite'
import { svelte } from '@sveltejs/vite-plugin-svelte'

// Port 1430 rather than the desktop app's 1421 so both can run at once —
// this app exists partly so other spikes can be driven in parallel, and a
// port collision the first time you try that would be a poor start.
export default defineConfig({
  plugins: [svelte()],
  clearScreen: false,
  server: {
    port: 1430,
    strictPort: true,
  },
  build: {
    target: 'esnext',
    // The window is the debugging surface for a protocol spike; a readable
    // stack trace is worth more than a smaller bundle.
    minify: false,
    sourcemap: true,
  },
})
