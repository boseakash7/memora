import { fileURLToPath } from 'node:url'
import { defineConfig } from 'vite'

// The TV page (`/tv` on the network listener): one plain script + stylesheet, built for old
// TV browsers (a 2019 Samsung TV runs roughly Chrome 63). The Rust server embeds `dist-tv/`.
export default defineConfig({
  root: fileURLToPath(new URL('./tv', import.meta.url)),
  base: '/tv/',
  publicDir: 'public',
  clearScreen: false,
  build: {
    outDir: fileURLToPath(new URL('./dist-tv', import.meta.url)),
    emptyOutDir: true,
    target: 'es2017',
    cssTarget: 'chrome61',
    lib: {
      entry: fileURLToPath(new URL('./tv/src/main.ts', import.meta.url)),
      formats: ['iife'],
      name: 'tv',
      fileName: () => 'tv.js',
      cssFileName: 'tv',
    },
  },
})
