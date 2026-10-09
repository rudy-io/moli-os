import { defineConfig } from 'vite';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import { existsSync } from 'node:fs';

// The dashboard's words live in locales/ at the repository's root: without
// them every text would show its key, so the build refuses to go on.
if (!existsSync(new URL('../locales/fr', import.meta.url))) {
  throw new Error('locales/fr is missing: the dashboard has no words (copy locales/ next to web/)');
}

// `MOLI_URL=http://192.168.1.x:8790 npm run dev` to develop against a live hub.
const target = process.env.MOLI_URL ?? 'http://127.0.0.1:8790';

export default defineConfig({
  plugins: [svelte()],
  build: { target: 'es2022', cssCodeSplit: false, reportCompressedSize: true },
  // The catalogues (locales/, at the repository's root) are shared with the server.
  server: { proxy: { '/api': { target, changeOrigin: true }, '/mcp': target }, fs: { allow: ['..'] } },
});
